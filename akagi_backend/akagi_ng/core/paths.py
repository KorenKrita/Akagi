import sys
from functools import cache
from pathlib import Path


@cache
def get_app_root() -> Path:
    if getattr(sys, "frozen", False):
        if hasattr(sys, "_MEIPASS"):
            return Path(sys._MEIPASS)
        return Path(sys.executable).parent
    return Path(__file__).resolve().parents[3]


@cache
def get_runtime_root() -> Path:
    """
    返回运行时数据目录（config、logs、lib、models）。
    - 开发模式：项目根目录
    - 生产模式（冻结包）：主程序所在根目录（Akagi-NG/）
    """
    if getattr(sys, "frozen", False):
        # 可执行文件位于 Akagi-NG/bin/akagi-ng.exe，向上两级即运行根目录
        return Path(sys.executable).parent.parent
    return get_app_root()


def get_assets_dir() -> Path:
    # assets 通过 PyInstaller --datas 与二进制一同打包
    return get_app_root() / "assets"


def get_settings_dir() -> Path:
    return get_runtime_root() / "config"


def get_lib_dir() -> Path:
    return get_runtime_root() / "lib"


def get_models_dir() -> Path:
    return get_runtime_root() / "models"


def get_logs_dir() -> Path:
    return get_runtime_root() / "logs"


def ensure_dir(path: Path) -> Path:
    path.mkdir(parents=True, exist_ok=True)
    return path
