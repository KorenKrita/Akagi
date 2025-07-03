import os
import time
import json
import random
from .logger import logger
from .util import Point
from settings.settings import settings

from functools import cmp_to_key
from mjai_bot.bot import AkagiBot


# Coordinates here is on the resolution of 16x9
LOCATION = {
    "tiles": [
        (2.23125  , 8.3625),
        (3.021875 , 8.3625),
        (3.8125   , 8.3625),
        (4.603125 , 8.3625),
        (5.39375  , 8.3625),
        (6.184375 , 8.3625),
        (6.975    , 8.3625),
        (7.765625 , 8.3625),
        (8.55625  , 8.3625),
        (9.346875 , 8.3625),
        (10.1375  , 8.3625),
        (10.928125, 8.3625),
        (11.71875 , 8.3625),
        (12.509375, 8.3625),
    ],
    "tsumo_space": 0.246875,
    "actions": [
        (10.875, 7), #none       #
        (8.6375, 7),             #   5   4   3
        (6.4   , 7),             #
        (10.875, 5.9),           #   2   1   0
        (8.6375, 5.9),           #
        (6.4   , 5.9),
        (10.875, 4.8),           # Not used
        (8.6375, 4.8),           # Not used
        (6.4   , 4.8),           # Not used
    ],
    "candidates": [
        (3.6625  , 6.3),         # (-(len/2)+idx+0.5)*2+5
        (4.49625 , 6.3),
        (5.33    , 6.3),
        (6.16375 , 6.3),
        (6.9975  , 6.3),
        (7.83125 , 6.3),         # 5 mid
        (8.665   , 6.3),
        (9.49875 , 6.3),
        (10.3325 , 6.3),
        (11.16625, 6.3),
        (12      , 6.3),
    ],
    "candidates_kan": [
        (4.325,   6.3),         #
        (5.4915,  6.3),
        (6.6583,  6.3),
        (7.825,   6.3),         # 3 mid
        (8.9917,  6.3),
        (10.1583, 6.3),
        (11.325,  6.3),
    ],
}

# Refer to majsoul2mjai.Operation
ACTION_PIORITY = [
    0,  # none      #
    99, # Discard   # There is no discard button
    4,  # Chi       # Opponent Discard
    3,  # Pon       # Opponent Discard
    3,  # Ankan     # Self Discard      # If Ankan and Kakan are both available, use only kakan.
    2,  # Daiminkan # Opponent Discard
    3,  # Kakan     # Self Discard
    2,  # Reach     # Self Discard
    1,  # Zimo      # Self Discard
    1,  # Rong      # Opponent Discard
    5,  # Ryukyoku  # Self Discard
    4,  # Nukidora  # Self Discard
]

ACTION2TYPE = {
    "none": 0,
    "chi": 2,
    "pon": 3,
    "daiminkan": 5,
    "hora": 9,
    #^^^^^^^^^^^^^^^^Opponent Discard^^^^^^^^^^^^^^^^
    "ryukyoku": 10,
    "nukidora": 11,
    "ankan": 4,
    "kakan": 6,
    "reach": 7,
    "zimo": 8,
    #^^^^^^^^^^^^^^^^Self Discard^^^^^^^^^^^^^^^^
}

TILES = [
    "1m",
    "2m",
    "3m",
    "4m",
    "5m",
    "6m",
    "7m",
    "8m",
    "9m",
    "1p",
    "2p",
    "3p",
    "4p",
    "5p",
    "6p",
    "7p",
    "8p",
    "9p",
    "1s",
    "2s",
    "3s",
    "4s",
    "5s",
    "6s",
    "7s",
    "8s",
    "9s",
    "E",
    "S",
    "W",
    "N",
    "P",
    "F",
    "C",
]

class AutoPlayMajsoul(object):
    def __init__(self):
        self.bot: AkagiBot = None

    def act(self, mjai_msg: dict) -> list[Point]:
        """
        Given a MJAI message, this method processes the message and returns list of coordinates to click.

        Args:
            mjai_msg (dict): The MJAI message to process.

        Returns:
            list[Point]: A list of Point objects representing the coordinates to click.
        """
        if mjai_msg is None:
            return []
        logger.debug(f"Act: {mjai_msg}")
        logger.debug(f"reach_accepted: {self.bot.self_riichi_accepted}")
        if mjai_msg['type'] == 'dahai' and not self.bot.self_riichi_accepted:
            random_time = random.uniform(1.0, 3.0)
            if not self.bot.last_kawa_tile:
                random_time = 3.0
            return_points = [Point(-1, -1, random_time)]
            return_points += self.click_dahai(mjai_msg)
            return return_points
        if mjai_msg['type'] == 'dahai' and self.bot.self_riichi_accepted:
            return []  # Do not click dahai when self riichi is accepted
        if mjai_msg['type'] in ['none', 'chi', 'pon', 'daiminkan', 'ankan', 'kakan', 'hora', 'reach', 'ryukyoku', 'nukidora']:
            random_time = random.uniform(1.0, 3.0)
            if not self.bot.last_kawa_tile:
                random_time = 3.0
            # kan can have multiple candidates too! ex: tehai=1111m 1111p 111s 11z, tsumohai=1s
            return_points = [Point(-1, -1, random_time)]
            return_points += self.click_chiponkan(mjai_msg)
            return return_points
        return []

    def click_chiponkan(self, mjai_msg: dict) -> list[Point]:
        # latest_operation_list_temp = self.latest_operation_list.copy()
        # latest_operation_list_temp.append({'type': 0, 'combination': []})
        # Avaliable operations
        return_points: list[Point] = []

        operation_list: list[int] = [0]
        if self.bot.can_discard:
            operation_list.append(1)
        if self.bot.can_chi:
            operation_list.append(2)
        if self.bot.can_pon:
            operation_list.append(3)
        if self.bot.can_ankan:
            operation_list.append(4)
        if self.bot.can_daiminkan:
            operation_list.append(5)
        if self.bot.can_kakan:
            operation_list.append(6)
        if self.bot.can_riichi:
            operation_list.append(7)
        if self.bot.can_tsumo_agari:
            operation_list.append(8)
        if self.bot.can_ron_agari:
            operation_list.append(9)
        if self.bot.can_ryukyoku:
            operation_list.append(10)
        # This does not check can nukidora after pon or kan
        # Might cause bugs in very rare cases
        if self.bot.tehai_vec34[9*3+3] > 0:
            operation_list.append(11)

        
        can_ankan = False
        can_kakan = False
        operation_list.sort(key=lambda x: ACTION_PIORITY[x])

        if sum(self.bot.tehai_vec34) in [14, 11, 8, 5, 2] and mjai_msg['type'] == 'hora':
            mjai_msg['type'] = 'zimo'

        for idx, operation in enumerate(operation_list):
            if operation == ACTION2TYPE[mjai_msg['type']]:
                return_points.append(
                    Point(
                        LOCATION['actions'][idx][0], 
                        LOCATION['actions'][idx][1], 
                        0.3
                    )
                )
                break

        if mjai_msg['type'] == 'reach':
            return return_points

        if mjai_msg['type'] in ['chi', 'pon', 'ankan', 'kakan']:
            consumed_pais_mjai = mjai_msg['consumed']
            consumed_pais_mjai = sorted(consumed_pais_mjai, key=cmp_to_key(compare_pai))
            if mjai_msg['type'] == 'chi':
                chi_candidates = self.bot.find_chi_consume_simple()
                if len(chi_candidates) == 1:
                    return return_points # No need to click
                chi_candidates = sorted(chi_candidates, key=cmp_to_key(compare_tehai))
                for idx, chi_candidate in enumerate(chi_candidates):
                    if consumed_pais_mjai == chi_candidate:
                        candidate_idx = int((-(len(chi_candidates)/2)+idx+0.5)*2+5)
                        return_points.append(
                            Point(
                                LOCATION['candidates'][candidate_idx][0], 
                                LOCATION['candidates'][candidate_idx][1], 
                                0.3
                            )
                        )
                        return return_points
            elif mjai_msg['type'] == 'pon':
                pon_candidates = self.bot.find_pon_consume_simple()
                if len(pon_candidates) == 1:
                    return return_points # No need to click
                # Theorem: len(pon_candidates) max is 2
                # We just click the second one no matter what
                candidate_idx = int((-(2/2)+1+0.5)*2+5)
                return_points.append(
                    Point(
                        LOCATION['candidates'][candidate_idx][0], 
                        LOCATION['candidates'][candidate_idx][1], 
                        0.3
                    )
                )
                return return_points
            # If both Ankan (type 4) and Kakan (type 6) are available, only one kan button will be shown, and candidates = [kakan, ankan]
            elif mjai_msg['type'] in ['ankan', 'kakan']:
                tehai34 = self.bot.tehai_vec34
                kan_candidates = [TILES[idx] for idx, val in enumerate(tehai34) if val >= 4]
                if len(kan_candidates) == 1:
                    return return_points
                consumed_pai = mjai_msg['pai'][0]
                if consumed_pai[-1] == 'r':
                    consumed_pai = consumed_pai[:2]
                for idx, pai in enumerate(kan_candidates):
                    if pai == consumed_pai:
                        candidate_idx = int((-(len(kan_candidates)/2)+idx+0.5)*2+3)
                        return_points.append(
                            Point(
                                LOCATION['candidates_kan'][candidate_idx][0], 
                                LOCATION['candidates_kan'][candidate_idx][1], 
                                0.3
                            )
                        )
                        return return_points
        return return_points

    def get_pai_coord(self, idx: int, tehais: list[str]):
        tehai_count = len(tehais)
        if idx == 13:
            pai_cord = (LOCATION['tiles'][tehai_count][0] + LOCATION['tsumo_space'], LOCATION['tiles'][tehai_count][1])
        else:
            pai_cord = LOCATION['tiles'][idx]

        return pai_cord

    def click_dahai(self, mjai_msg: dict) -> list[Point]:
        dahai = mjai_msg['pai']
        # In Majsoul, if you are the first dealer, there is no tsumohai, but 14 tehai.
        # However, in MJAI, there is 13 tehai and 1 tsumohai.
        # We've already sorted tehai in ./mitm/bridge/majsoul/bridge.py:L225
        # So we can assume that the last tile is the tsumohai.
        tehai = self.bot.tehai_mjai
        tsumohai = self.bot.last_self_tsumo
        is_tsumohai = False
        if len(tehai) in [14, 11, 8, 5, 2] and tsumohai != "":
            tehai.remove(tsumohai)
            is_tsumohai = True

        tehai = sorted(tehai, key=cmp_to_key(compare_pai))

        return_points: list[Point] = []

        if is_tsumohai:
            if dahai == tsumohai:
                pai_coord = self.get_pai_coord(13, tehai)
                return_points.append(
                    Point(
                        pai_coord[0], 
                        pai_coord[1], 
                        0.3
                    )
                )
                return return_points
        for i in range(13):
            if i >= len(tehai):
                logger.debug(f"i >= len(tehai): {i} >= {len(tehai)}")
                break
            if dahai == tehai[i]:
                pai_coord = self.get_pai_coord(i, tehai)
                return_points.append(
                    Point(
                        pai_coord[0], 
                        pai_coord[1], 
                        0.3
                    )
                )
                return return_points
        
# Custom compare function for lists of pai
def compare_tehai(hand1: list[str], hand2: list[str]) -> int:
    # Compare element by element
    for p1, p2 in zip(hand1, hand2):
        cmp = compare_pai(p1, p2)
        if cmp != 0:
            return cmp
    # If all compared elements are equal, compare the lengths
    if len(hand1) > len(hand2):
        return 1
    elif len(hand1) < len(hand2):
        return -1
    else:
        return 0

def compare_pai(pai1: str, pai2: str):
    # Smallest
    # 1m~4m, 5mr, 5m~9m,
    # 1p~4p, 5pr, 5p~9p,
    # 1s~4s, 5sr, 5s~9s,
    # E, S, W, N, P, F, C, ?
    # Biggest
    pai_order = [
        '1m', '2m', '3m', '4m', '5mr', '5m', '6m', '7m', '8m', '9m',
        '1p', '2p', '3p', '4p', '5pr', '5p', '6p', '7p', '8p', '9p',
        '1s', '2s', '3s', '4s', '5sr', '5s', '6s', '7s', '8s', '9s',
        'E', 'S', 'W', 'N', 'P', 'F', 'C', '?'
    ]
    idx1 = pai_order.index(pai1)
    idx2 = pai_order.index(pai2)
    if idx1 > idx2:
        return 1  
    elif idx1 == idx2:
        return 0
    else:
        return -1