import json
from dataclasses import Field, fields
from functools import cache

import numpy as np

from akagi_ng.schema.constants import MahjongConstants
from akagi_ng.schema.types import MJAIEvent, MJAIEventBase, MJAIMetadata

# fmt: off
mask_unicode_4p = [
    *MahjongConstants.BASE_TILES,
    "reach", "chi_low", "chi_mid", "chi_high", "pon", "kan_select", "hora", "ryukyoku", "none"
]
mask_unicode_3p = [
    *MahjongConstants.BASE_TILES,
    "reach", "pon", "kan_select", "nukidora", "hora", "ryukyoku", "none"
]
# fmt: on


def _is_approximately_equal(left: float, right: float) -> bool:
    """检查两个浮点数是否近似相等"""
    return np.abs(left - right) <= np.finfo(float).eps


def _softmax(arr: list[float] | np.ndarray, temperature: float) -> np.ndarray:
    """应用 softmax 变换到数组。使用数值稳定的平移方法。"""
    arr = np.array(arr, dtype=float)

    if arr.size == 0:
        return arr

    if not _is_approximately_equal(temperature, 1.0):
        arr /= temperature

    # 平移值以确保数值稳定性
    max_val = np.max(arr)
    e_x = np.exp(arr - max_val)
    return e_x / e_x.sum()


def meta_to_recommend(meta: MJAIMetadata, is_3p: bool, temperature: float) -> list[tuple[str, float]]:
    """将元数据转换为排序后的推荐列表。使用压缩的 zip 遍历以提升性能。"""
    mask_unicode = mask_unicode_3p if is_3p else mask_unicode_4p

    q_values = meta.get("q_values")
    mask_bits = meta.get("mask_bits", 0)
    if not q_values:
        return []

    scaled_q_values = _softmax(q_values, temperature)

    # 提取被标记位激活的标签
    active_labels = [label for i, label in enumerate(mask_unicode) if mask_bits & (1 << i)]

    # 将标签与计算出的概率合并后排序
    recommend = list(zip(active_labels, scaled_q_values.tolist(), strict=False))
    recommend.sort(key=lambda x: x[1], reverse=True)
    return recommend


@cache
def _get_dataclass_fields(cls: type[MJAIEventBase]) -> tuple[Field[object], ...]:
    """缓存 MJAI 事件数据类的字段对象。"""
    return fields(cls)


def serialize_mjai_event(event: MJAIEvent) -> str:
    """使用紧凑的 JSON 格式序列化 MJAI 事件，通过类获取缓存的 dataclass 字段。"""
    payload = {f.name: getattr(event, f.name) for f in _get_dataclass_fields(event.__class__)}
    return json.dumps(payload, separators=(",", ":"))
