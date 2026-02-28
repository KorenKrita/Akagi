import json

from akagi_ng.mjai_bot.engine.factory import load_bot_and_engine
from akagi_ng.mjai_bot.logger import logger
from akagi_ng.mjai_bot.lookahead import LookaheadBot
from akagi_ng.mjai_bot.status import BotStatusContext
from akagi_ng.mjai_bot.utils import meta_to_recommend, serialize_mjai_event
from akagi_ng.schema.notifications import NotificationCode
from akagi_ng.schema.protocols import EngineProtocol, MJAIBotProtocol
from akagi_ng.schema.types import (
    EndGameEvent,
    MJAIEvent,
    MJAIEventBase,
    MJAIMetadata,
    MJAIResponse,
    ReachEvent,
    StartGameEvent,
    StartKyokuEvent,
)


class MortalBot:
    """
    MJAI Bot 的封装类,负责处理事件并返回推荐动作。
    """

    def __init__(
        self,
        status: BotStatusContext,
        engine: EngineProtocol | None = None,
        is_3p: bool = False,
    ):
        self.status = status
        self.engine = engine
        self.is_3p = is_3p
        self.player_id: int | None = None
        self.history: list[MJAIEvent] = []
        self.bot: MJAIBotProtocol | None = None
        self.game_start_event: StartGameEvent | None = None

        self.logger = logger

    def react(self, event: MJAIEvent) -> MJAIResponse | None:
        """MortalBot 对外核心接口，流水线处理事件"""
        try:
            # 1. 预处理：生命周期管理与历史记录
            self._pre_react(event)

            # 2. 决策：调用模型/引擎
            response: MJAIResponse | None = self._think(event)
            if not response:
                return None

            # 3. 增强：注入元数据与执行前瞻逻辑
            if not (meta := response.get("meta")):
                return None

            self._post_react(meta)

            # 三麻抑制单一选项的 meta 显示
            if self.is_3p and response.get("type") == "none":
                response.pop("meta", None)

            return response

        except Exception as e:
            self.logger.exception(f"MortalBot runtime error in select_action: {e}")
            self.status.set_flag(NotificationCode.BOT_RUNTIME_ERROR)
            return None

    def _pre_react(self, event: MJAIEvent) -> None:
        """维护历史、处理生命周期事件。"""
        match event:
            case StartGameEvent():
                self._handle_start_game(event)
            case StartKyokuEvent():
                self.history = []
            case EndGameEvent():
                self._handle_end_game()

        # 维护历史
        self.history.append(event)

    def _think(self, event: MJAIEvent) -> MJAIResponse | None:
        """调用引擎/模型获取决策动作"""
        if not self.bot:
            return None

        is_sync = False
        match event:
            case MJAIEventBase(sync=is_sync):
                pass

        try:
            # MJAI 协议底层 C++ Bot (mjai-python) 接受并返回 JSON 字符串
            event_json = serialize_mjai_event(event)
            # 同步快进：仅更新 C++ 状态机，不触发决策推理。
            res = self.bot.react(event_json, can_act=False) if is_sync else self.bot.react(event_json)
            if not res:
                return None
            try:
                return json.loads(res)
            except json.JSONDecodeError:
                self.logger.error(f"MortalBot: engine returned invalid JSON: {res}")
                self.status.set_flag(NotificationCode.JSON_DECODE_ERROR)
                return None
        except Exception:
            self.logger.exception("MortalBot engine error")
            self.status.set_flag(NotificationCode.BOT_RUNTIME_ERROR)
            return None

    def _post_react(self, meta: MJAIMetadata):
        """元数据增强阶段"""
        # 1. 注入同步元数据
        meta.update(self.status.metadata)

        # 2. 立直前瞻逻辑
        self._handle_riichi_lookahead(meta)

    def _handle_start_game(self, e: StartGameEvent):
        """处理游戏开始事件，初始化模型和引擎"""
        self.player_id = e.id
        self.bot, self.engine = load_bot_and_engine(self.status, self.player_id, self.is_3p)
        self.history = []
        self.game_start_event = e

        # 检测加载的模型类型并设置通知
        if self.engine:
            engine_meta = self.status.metadata
            engine_type = engine_meta.get(NotificationCode.ENGINE_TYPE, "unknown")

            match engine_type:
                case "akagiot":
                    self.status.set_flag(NotificationCode.MODEL_LOADED_ONLINE)
                case "mortal":
                    self.status.set_flag(NotificationCode.MODEL_LOADED_LOCAL)
                case _:
                    self.logger.warning(f"Unknown engine type: {engine_type}")

    def _handle_end_game(self):
        """处理游戏结束事件，清理状态"""
        self.player_id = None
        self.bot = None
        self.engine = None
        self.game_start_event = None

    def _handle_riichi_lookahead(self, meta: MJAIMetadata):
        """
        处理立直前瞻逻辑
        """
        if "q_values" not in meta or "mask_bits" not in meta:
            return

        recommendations = meta_to_recommend(meta, is_3p=self.is_3p)
        top_3_actions = [rec[0] for rec in recommendations[:3]]

        if "reach" not in top_3_actions:
            return

        self.logger.info(f"Riichi Lookahead: Reach is in Top 3 ({top_3_actions}). Starting simulation.")
        lookahead_meta = self._run_riichi_lookahead()
        if lookahead_meta:
            meta["riichi_lookahead"] = lookahead_meta
        else:
            self.status.set_flag(NotificationCode.RIICHI_SIM_FAILED)

    def _run_riichi_lookahead(self) -> MJAIMetadata | None:
        """
        运行立直前瞻模拟。
        """
        try:
            if not self.engine or self.player_id is None:
                return None

            self.logger.debug("Riichi Lookahead: Starting simulation (using LookaheadBot).")
            sim_status = BotStatusContext()
            sim_engine = self.engine.fork(status=sim_status)
            lookahead_bot = LookaheadBot(sim_engine, self.player_id, is_3p=self.is_3p)

            reach_event = ReachEvent(actor=self.player_id)
            sim_meta: MJAIMetadata | None = lookahead_bot.simulate_reach(
                self.history,
                reach_event,
                game_start_event=self.game_start_event,
            )

            if not sim_meta:
                self.logger.warning("Riichi Lookahead: Simulation returned no metadata.")
                return None

            sim_recs = meta_to_recommend(sim_meta, is_3p=self.is_3p)
            all_candidates = ", ".join([f"{action}({conf:.3f})" for action, conf in sim_recs])
            self.logger.info(f"Riichi Lookahead: Simulation success. Candidates: {all_candidates}")
            return sim_meta

        except Exception:
            self.logger.exception("Riichi Lookahead failed")
            return None
