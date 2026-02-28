"""
测试模块：akagi_backend/tests/conftest.py

描述：项目顶层测试配置，包含全局 fixtures、钩子函数和测试标记推断逻辑。
主要测试点：
- 测试标记 (markers) 的自动推断逻辑。
- 提供通用的游戏消息、Liqi 协议消息和各平台 Bridge 的模拟 fixture。
"""

from pathlib import Path
from unittest.mock import MagicMock

import pytest

from akagi_ng.bridge import AmatsukiBridge, MajsoulBridge, RiichiCityBridge, TenhouBridge

# --- libriichi 可用性检测 ---
try:
    from akagi_ng.core.lib_loader import libriichi  # noqa: F401

    HAS_LIBRIICHI = True
except ImportError:
    HAS_LIBRIICHI = False

try:
    from akagi_ng.core.lib_loader import libriichi3p  # noqa: F401

    HAS_LIBRIICHI3P = True
except ImportError:
    HAS_LIBRIICHI3P = False


def _infer_domain_markers(path: Path) -> set[str]:
    """根据测试文件名推断领域标签。"""
    stem = path.stem.lower()
    markers: set[str] = set()

    keyword_map: dict[str, tuple[str, ...]] = {
        "bridge": ("bridge", "majsoul", "tenhou", "riichi_city", "amatsuki", "liqi", "nukidora"),
        "engine": ("engine", "akagi_ot"),
        "bot": ("bot", "lookahead", "state_tracker"),
        "application": ("application", "controller", "settings", "utils"),
        "dataserver": ("dataserver", "sse"),
        "client": ("electron", "mitm_client", "mitm_bridge"),
    }

    for marker, keywords in keyword_map.items():
        if any(keyword in stem for keyword in keywords):
            markers.add(marker)
    return markers


def pytest_collection_modifyitems(items: list[pytest.Item]):
    """统一给单测/集成测试打标签，便于分组执行与回归编排。"""
    for item in items:
        path = Path(str(item.fspath))
        norm = path.as_posix()

        if "/tests/unit/" in norm:
            item.add_marker("unit")
        elif "/tests/integration/" in norm:
            item.add_marker("integration")

        for marker in _infer_domain_markers(path):
            item.add_marker(marker)


@pytest.fixture
def mock_flow():
    """创建一个模拟的 HTTPFlow 对象"""
    flow = MagicMock()
    flow.id = "test_flow_id"
    flow.request.url = "wss://example.com"
    return flow


@pytest.fixture
def sample_start_game_message():
    """示例开始游戏消息"""
    return {
        "type": "start_game",
        "id": 0,
        "is_3p": False,
    }


@pytest.fixture
def sample_liqi_auth_game_req():
    """示例 Liqi authGame 请求消息"""
    return {
        "method": ".lq.FastTest.authGame",
        "type": 2,
        "data": {"accountId": 12345},
    }


@pytest.fixture
def sample_liqi_auth_game_res_4p():
    """示例 Liqi authGame 响应消息（4人麻将）"""
    return {
        "method": ".lq.FastTest.authGame",
        "type": 3,
        "data": {
            "seatList": [1, 2, 3, 4],
            "gameConfig": {"meta": {"modeId": 1}},
        },
    }


@pytest.fixture
def sample_liqi_auth_game_res_3p():
    """示例 Liqi authGame 响应消息（3人麻将）"""
    return {
        "method": ".lq.FastTest.authGame",
        "type": 3,
        "data": {
            "seatList": [1, 2, 3],
            "gameConfig": {"meta": {"modeId": 11}},
        },
    }


# --- Shared Data & Mocks ---


@pytest.fixture
def sample_tehai_strs():
    """示例手牌（字符串格式）"""
    return ["1m", "2m", "3m", "4m", "5m", "6m", "7m", "8m", "9m", "1p", "2p", "3p", "4p"]


@pytest.fixture
def sample_tehai_indices():
    """示例手牌（索引格式）"""
    return [0, 1, 2, 4, 5, 8, 9, 10, 12, 13, 16, 17, 20]


# --- Bridge Fixtures ---


@pytest.fixture
def majsoul_bridge():
    """创建一个干净的 MajsoulBridge 实例"""
    return MajsoulBridge()


@pytest.fixture
def amatsuki_bridge(sample_tehai_strs):
    """创建一个干净的 AmatsukiBridge 实例"""
    bridge = AmatsukiBridge()
    bridge.valid_flow = True
    bridge.seat = 0
    bridge.desk_id = 123
    bridge.game_status = MagicMock()
    bridge.game_status.tehai = sample_tehai_strs
    bridge.game_status.is_3p = False
    return bridge


@pytest.fixture
def riichi_city_bridge():
    """创建一个干净的 RiichiCityBridge 实例"""
    bridge = RiichiCityBridge()
    bridge.uid = 1001
    bridge.game_status = MagicMock()  # 这里必须使用 MagicMock，便于动态属性注入
    bridge.game_status.accept_reach = None
    bridge.game_status.dora_markers = []
    bridge.game_status.player_list = [1000, 1001, 1002, 1003]  # 当前用户位于索引 1
    bridge.game_status.seat = 1
    bridge.game_status.last_dahai_actor = 0
    bridge.game_status.is_3p = False
    return bridge


@pytest.fixture
def tenhou_bridge(sample_tehai_indices):
    """创建一个干净的 TenhouBridge 实例"""
    bridge = TenhouBridge()
    bridge.state = MagicMock()
    bridge.state.seat = 0
    bridge.state.hand = sample_tehai_indices
    bridge.state.is_3p = False
    return bridge
