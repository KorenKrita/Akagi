from dataclasses import dataclass, field

from akagi_ng.mjai_bot.logger import logger
from akagi_ng.mjai_bot.status import BotStatusContext
from akagi_ng.mjai_bot.utils import serialize_mjai_event
from akagi_ng.schema.constants import MahjongConstants
from akagi_ng.schema.notifications import NotificationCode
from akagi_ng.schema.protocols import PlayerStateProtocol, StateTrackerProtocol
from akagi_ng.schema.types import (
    DahaiEvent,
    MJAIEvent,
    MJAIMetadata,
    MJAIResponse,
    NukidoraEvent,
    StartGameEvent,
)


@dataclass
class StateTracker(StateTrackerProtocol):
    """
    状态追踪器，用于跟踪游戏状态。
    作为 libriichi PlayerState 的封装包装器，向下提供状态查询以供推理、副露推荐以及前端展示等使用。
    """

    status: BotStatusContext
    is_3p: bool = False
    meta: MJAIMetadata = field(default_factory=dict)
    player_id: int = 0
    player_state: PlayerStateProtocol | None = None
    discardable_tiles_riichi_declaration: list[str] = field(default_factory=list)

    def react(self, event: MJAIEvent) -> MJAIResponse | None:
        try:
            processed_event = event
            match event:
                case StartGameEvent(id=player_id, is_3p=is_3p):
                    self.player_id = player_id
                    self.is_3p = is_3p

                    from akagi_ng.core.lib_loader import libriichi as libs

                    self.player_state = libs.state.PlayerState(self.player_id)
                # 三麻兼容：libriichi.PlayerState 状态追踪库不支持 nukidora 事件，需要转换为 dahai 事件
                case NukidoraEvent(actor=actor):
                    processed_event = DahaiEvent(
                        actor=actor,
                        pai="N",
                        tsumogiri=self.last_self_tsumo == "N" and actor == self.player_id,
                    )
                case _:
                    pass

            logger.debug(f"-> {processed_event}")
            if self.player_state:
                self.player_state.update(serialize_mjai_event(processed_event))

            return None

        except Exception:
            brief = self.player_state.brief_info() if self.player_state else "None"
            logger.exception(f"Exception in react. Brief info:\n{brief}")
            self.status.set_flag(NotificationCode.STATE_TRACKER_ERROR)

        return None

    @property
    def last_self_tsumo(self) -> str | None:
        return self.player_state.last_self_tsumo() if self.player_state else None

    @property
    def last_kawa_tile(self) -> str | None:
        return self.player_state.last_kawa_tile() if self.player_state else None

    @property
    def self_riichi_accepted(self) -> bool:
        return self.player_state.self_riichi_accepted if self.player_state else False

    @property
    def can_tsumo_agari(self) -> bool:
        return self.player_state.last_cans.can_tsumo_agari if self.player_state else False

    @property
    def tehai_mjai_with_aka(self) -> list[str]:
        """根据 tehai 和 akas_in_hand 构建带有赤宝牌标记的手牌列表"""
        if not self.player_state:
            return []

        tiles = MahjongConstants.BASE_TILES[:34]

        counts = self.player_state.tehai
        akas = self.player_state.akas_in_hand

        result = []
        for i, count in enumerate(counts):
            if count == 0:
                continue
            base_str = tiles[i]

            # 处理赤宝牌
            aka_idx = -1
            match base_str:
                case "5m":
                    aka_idx = 0
                case "5p":
                    aka_idx = 1
                case "5s":
                    aka_idx = 2

            non_aka_count = count
            if aka_idx >= 0 and akas[aka_idx]:
                result.append(f"{base_str}r")
                non_aka_count -= 1

            for _ in range(non_aka_count):
                result.append(base_str)

        return result
