import queue

from akagi_ng.electron_client.base import BaseElectronClient
from akagi_ng.electron_client.majsoul import MajsoulElectronClient
from akagi_ng.electron_client.tenhou import TenhouElectronClient
from akagi_ng.schema.constants import Platform
from akagi_ng.schema.types import AkagiEvent


def create_electron_client(platform: Platform, shared_queue: queue.Queue[AkagiEvent]) -> BaseElectronClient | None:
    """
    按平台创建对应的 ElectronClient。
    """
    if platform == Platform.MAJSOUL:
        return MajsoulElectronClient(shared_queue=shared_queue)

    if platform == Platform.TENHOU:
        return TenhouElectronClient(shared_queue=shared_queue)

    # AUTO 模式默认走雀魂客户端
    if platform == Platform.AUTO:
        return MajsoulElectronClient(shared_queue=shared_queue)

    # 其他平台仅支持 MITM，因此返回 None
    return None


__all__ = [
    "BaseElectronClient",
    "MajsoulElectronClient",
    "TenhouElectronClient",
    "create_electron_client",
]
