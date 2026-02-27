import numpy as np

from akagi_ng.schema.constants import MahjongConstants
from akagi_ng.schema.types import MJAIMetadata

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


def _softmax(arr: list[float] | np.ndarray, temperature: float = 1.0) -> np.ndarray:
    """应用 softmax 变换到数组"""
    arr = np.array(arr, dtype=float)

    if arr.size == 0:
        return arr

    if not _is_approximately_equal(temperature, 1.0):
        arr /= temperature

    # 平移值以确保数值稳定性
    max_val = np.max(arr)
    arr = arr - max_val

    # 应用 softmax 变换
    exp_arr = np.exp(arr)
    sum_exp = np.sum(exp_arr)

    return exp_arr / sum_exp


def meta_to_recommend(meta: MJAIMetadata, is_3p: bool = False, temperature: float = 1.0) -> list[tuple[str, float]]:
    recommend = []
    mask_unicode = mask_unicode_3p if is_3p else mask_unicode_4p

    q_values = meta["q_values"]
    mask_bits = meta["mask_bits"]
    scaled_q_values = _softmax(q_values, temperature)

    q_idx = 0
    for i in range(len(mask_unicode)):
        if (mask_bits & (1 << i)) != 0:
            if q_idx < len(scaled_q_values):
                recommend.append((mask_unicode[i], float(scaled_q_values[q_idx])))
                q_idx += 1
            else:
                break

    return sorted(recommend, key=lambda x: x[1], reverse=True)
