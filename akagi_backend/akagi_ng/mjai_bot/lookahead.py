import json

from akagi_ng.mjai_bot.logger import logger
from akagi_ng.mjai_bot.utils import serialize_mjai_event
from akagi_ng.schema.notifications import NotificationCode
from akagi_ng.schema.protocols import EngineProtocol
from akagi_ng.schema.types import MJAIEvent, MJAIMetadata, StartGameEvent


class LookaheadBot:
    """
    专门用于立直前瞻（Lookahead）的 Bot。
    它不维护长期的游戏状态，而是通过快速重放历史事件来恢复状态，
    并对候选切牌进行模拟推理。
    """

    def __init__(self, engine: EngineProtocol, player_id: int, is_3p: bool = False):
        self.engine = engine
        self.player_id = player_id
        self.is_3p = is_3p

    def simulate_reach(
        self,
        history_events: list[MJAIEvent],
        candidate_event: MJAIEvent,
        game_start_event: StartGameEvent | None = None,
    ) -> MJAIMetadata | None:
        """
        模拟立直后的行为（一发/自摸等）。
        在当前状态下模拟 Reach, 并返回 meta 数据(含 q_values/mask_bits)。

        Args:
            history_events: 当前局的历史事件（start_kyoku 之后的事件）
            candidate_event: 候选的 reach 事件
            game_start_event: 游戏开始事件，用于初始化 C++ Bot 状态
        """
        # 1. 为模拟创建一个专用的 C++ Bot 实例
        # 直接使用 fork 出来的真实引擎。回放阶段通过 can_act=False 避免推理。
        if self.is_3p:
            from akagi_ng.core.lib_loader import libriichi3p as libs
        else:
            from akagi_ng.core.lib_loader import libriichi as libs

        sim_bot = libs.mjai.Bot(self.engine, self.player_id)

        # 2. 重放历史事件
        # 回放阶段通过 can_act=False 仅推进状态，不触发推理。
        all_events: list[MJAIEvent] = []
        if game_start_event:
            all_events.append(game_start_event)
        all_events.extend(history_events)

        for e in all_events:
            e_json = serialize_mjai_event(e)
            try:
                sim_bot.react(e_json, can_act=False)
            except Exception:
                logger.exception(f"LookaheadBot: Replay failed at event {e_json}")
                return None

        # 3. 执行候选事件（真正的推理）
        cand_json = serialize_mjai_event(candidate_event)

        try:
            response_json = sim_bot.react(cand_json)

            if response_json:
                try:
                    response = json.loads(response_json)
                except json.JSONDecodeError:
                    logger.error(f"LookaheadBot: engine returned invalid JSON: {response_json}")
                    self.engine.status.set_flag(NotificationCode.JSON_DECODE_ERROR)
                    return None
                meta: MJAIMetadata = response.get("meta", {})
                if meta:
                    return meta
                logger.warning("LookaheadBot: engine returned empty meta.")

        except Exception:
            logger.exception("LookaheadBot: sim_bot.react failed")

        return None
