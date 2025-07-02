from __future__ import annotations

import loguru
from loguru import logger as main_logger
from datetime import datetime
from pathlib import Path

# Log to: "./Logs/rpc_client_<timestamp>.log"
log_path = Path().cwd() / "logs" / f"rpc_client_{datetime.now().strftime('%Y%m%d_%H%M%S')}.log"
logger: loguru.Logger = main_logger.bind(module="rpc_client")
main_logger.add(log_path, level="DEBUG", filter=lambda record: record["extra"].get("module") == "rpc_client")

import time
import queue
import asyncio
import threading
from settings.settings import settings
from .majsoul import PlaywrightController, mjai_messages


class Client(object):
    def __init__(self):
        self.messages: queue.Queue[dict] = None
        self.running = False
        self._thread: threading.Thread = None
        self.controller: PlaywrightController = PlaywrightController(settings.majsoul_url)

    def start(self):
        if self.running:
            return
        self.messages = mjai_messages
        self._thread = threading.Thread(target=self.controller.start, daemon=True)
        self._thread.start()
        self.running = True

    def stop(self):
        if not self.running:
            return
        if not self.controller.running:
            return
        self.controller.stop()
        self.messages = None
        self.running = False
        self._thread.join()
        self._thread = None

    def send_command(self, command: dict):
        if not self.running:
            raise RuntimeError("Client is not running.")
        if not self.controller.running:
            raise RuntimeError("Controller is not running.")
        logger.debug(f"Sending command: {command}")
        self.controller.command_queue.put(command)

    def dump_messages(self) -> list[dict]:
        ans: list[dict] = []
        while not self.messages.empty():
            message = self.messages.get()
            logger.debug(f"Message: {message}")
            ans.append(message)
        return ans
