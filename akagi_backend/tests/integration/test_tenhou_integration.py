"""
测试模块：akagi_backend/tests/integration/test_tenhou_integration.py

描述：针对天凤 (Tenhou) 平台的完整对局协议流转集成测试。
主要测试点：
- HELO、TAIKYOKU、INIT 等天凤标签的序列化模拟与接收转发。
- 自动化处理天凤复杂的 JSON 结构化日志。
- 验证天凤 Bridge 与 Controller 在处理摸切、打牌决策时的完整集成逻辑。
"""

import json

import pytest


@pytest.mark.integration
def test_tenhou_bridge_full_flow(tenhou_bridge, integration_controller):
    """测试 Tenhou Bridge 解析消息并由 Controller 处理的流程"""
    # 1. HELO 消息 (JSON 格式)
    helo_msg = json.dumps({"tag": "HELO", "name": "User", "tid": "0", "sx": "M"}).encode("utf-8")

    events = tenhou_bridge.parse(helo_msg)
    # HELO 不产生 MJAI 事件，仅初始化
    assert events == []

    # 2. TAIKYOKU 消息 (start_game)
    taikyoku_msg = json.dumps({"tag": "TAIKYOKU", "oya": "0"}).encode("utf-8")
    events = tenhou_bridge.parse(taikyoku_msg)
    assert len(events) == 1
    assert events[0].type == "start_game"

    # Controller 处理 start_game
    integration_controller.react(events[0])
    assert integration_controller.last_response is None

    # 3. INIT 消息 (start_kyoku)
    init_msg = json.dumps(
        {
            "tag": "INIT",
            "seed": "0,0,0,0,0,4",
            "ten": "250,250,250,250",
            "oya": "0",
            "hai": "0,4,8,12,16,20,24,28,32,36,40,44,48",
        }
    ).encode("utf-8")

    events = tenhou_bridge.parse(init_msg)
    assert len(events) == 1
    assert events[0].type == "start_kyoku"

    # Controller 处理 start_kyoku
    # 这会尝试加载 Bot
    integration_controller.react(events[0])
    res = integration_controller.last_response
    assert res is None or isinstance(res, dict)

    # 4. T 消息 (tsumo)
    # Tenhou JSON logs wrap tags in JSON objects
    tsumo_msg = json.dumps({"tag": "T52"}).encode("utf-8")
    events = tenhou_bridge.parse(tsumo_msg)
    assert len(events) == 1
    assert events[0].type == "tsumo"

    # Controller 处理 tsumo
    integration_controller.react(events[0])
    res = integration_controller.last_response
    # 应该有响应（dahai 或者 none，取决于 Bot）
    assert res is None or "type" in res
