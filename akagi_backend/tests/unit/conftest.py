"""
测试模块：akagi_backend/tests/unit/conftest.py

描述：单元测试专属配置，主要用于提供单元测试环境下的通用 mock。
主要测试点：
- 模拟 lib_loader 模块以隔离真实的 C++ 动态库加载。
"""

import sys
from unittest.mock import MagicMock, patch

import pytest


@pytest.fixture
def mock_lib_loader_module():
    """Mock lib_loader 模块，防止加载真实二进制库"""
    mock_module = MagicMock()
    mock_module.libriichi = MagicMock()
    mock_module.libriichi.mjai.Bot = MagicMock
    mock_module.libriichi3p = MagicMock()
    mock_module.libriichi3p.mjai.Bot = MagicMock

    with patch.dict(sys.modules, {"akagi_ng.core.lib_loader": mock_module}):
        yield mock_module
