from .util import Point
from .logger import logger
from .autoplay_majsoul import AutoPlayMajsoul
from settings.settings import settings
from playwright_client.client import Client
    
class AutoPlay(object):
    def __init__(self):
        self.autoplay: AutoPlayMajsoul = AutoPlayMajsoul()
        self.client: Client = None

    def set_bot(self, bot):
        """
        Args:
            bot (AkagiBot): The AkagiBot instance to be used.

        Returns:
            None: No return value.
        """
        self.autoplay.bot = bot

    def set_client(self, client: Client):
        """
        Args:
            client (Client): The Client instance to be used.

        Returns:
            None: No return value.
        """
        self.client = client

    def act(self, mjai_msg: dict) -> bool:
        """
        Given a MJAI message, this method processes the message and performs the corresponding action.

        Args:
            mjai_msg (dict): The MJAI message to process.

        Returns:
            bool: True if the action was performed, False otherwise.
        """
        if not self.client.running:
            logger.error("Client is not running.")
            return False
        points: list[Point] = self.autoplay.act(mjai_msg)
        if not points:
            return False
        for point in points:
            command = {"command": "delay", "delay": point.delay}
            self.client.send_command(command)
            command = {"command": "click", "point": [point.x, point.y]}
            self.client.send_command(command)
        logger.debug(f"Processed MJAI message: {mjai_msg}")
        logger.debug(f"Points to click: {points}")
        return True
