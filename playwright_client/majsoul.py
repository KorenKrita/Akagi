import threading
import traceback
import asyncio
import queue
import time
import mitmproxy.http
import mitmproxy.log
import mitmproxy.tcp
import mitmproxy.websocket
from pathlib import Path
from mitmproxy import proxy, options, ctx
from mitmproxy.tools.dump import DumpMaster
from playwright.sync_api import sync_playwright, Playwright, Browser, Page, WebSocket
from .bridge import MajsoulBridge
from .mitm_abc import ClientWebSocketABC
from .logger import logger

# Because in Majsouls, every flow's message has an id, we need to use one bridge for each flow
activated_flows: list[str] = [] # store all flow.id ([-1] is the recently opened)
majsoul_bridges: dict[str, MajsoulBridge] = {} # store all flow.id -> MajsoulBridge
mjai_messages: queue.Queue[dict] = queue.Queue() # store all messages


class PlaywrightController:
    """
    A controller for a Playwright browser instance that runs in a separate thread.
    It manages a single page, processes commands from a queue, monitors WebSockets,
    and handles clicking based on a normalized 16x9 grid.
    """

    def __init__(self, url: str):
        """
        Initializes the controller.

        Args:
            url (str): The fixed URL the browser page will navigate to.
            mjai_queue (queue.Queue): The queue to put parsed MJAI messages into.
        """
        self.url = url
        self.command_queue: queue.Queue[dict] = queue.Queue()
        self._stop_event = threading.Event()
        self.running = False

        self.playwright: Playwright | None = None
        self.browser: Browser | None = None
        self.page: Page | None = None

        self.bridge_lock = threading.Lock()

    def _on_web_socket(self, ws: WebSocket):
        """
        Callback for new WebSocket connections. Equivalent to `websocket_start`.
        """
        global majsoul_bridges
        logger.info(f"[WebSocket] Connection opened")
        logger.info(f"[WebSocket] Connection opened: {ws.url}")
        
        # Create and store a bridge for this new WebSocket flow
        majsoul_bridges[ws] = MajsoulBridge()

        # Set up listeners for messages and closure on this specific WebSocket instance
        ws.on("framesent", lambda payload: self._on_frame(ws, payload, from_client=True))
        ws.on("framereceived", lambda payload: self._on_frame(ws, payload, from_client=False))
        ws.on("close", lambda: self._on_socket_close(ws))

    def _on_frame(self, ws: WebSocket, payload: str | bytes, from_client: bool):
        """
        Callback for WebSocket messages. Equivalent to `websocket_message`.
        """
        global mjai_messages, majsoul_bridges
        direction = "<- Sent" if from_client else "-> Received"
        logger.debug(f"[WebSocket] {direction}: {payload}")

        bridge = majsoul_bridges.get(ws)
        if not bridge:
            logger.error(f"[WebSocket] Message received from untracked WebSocket: {ws.url}")
            return
        
        try:
            # Acquire lock to ensure thread-safe parsing
            with self.bridge_lock:
                msgs = bridge.parse(payload)
            
            if msgs is None:
                return
            
            # Add parsed messages to the shared queue
            for m in msgs:
                mjai_messages.put(m)
        except Exception:
            # The 'with' statement handles lock release even on error
            logger.error(f"[WebSocket] Error during message parsing: {traceback.format_exc()}")

    def _on_socket_close(self, ws: WebSocket):
        """
        Callback for WebSocket closures. Equivalent to `websocket_end`.
        """
        global majsoul_bridges
        if ws in majsoul_bridges:
            logger.info(f"[WebSocket] Connection closed: {ws.url}")
            # Clean up the bridge for the closed WebSocket
            del majsoul_bridges[ws]
        else:
            logger.warning(f"[WebSocket] Untracked WebSocket connection closed: {ws.url}")

    def _get_clickxy(self, x: float, y: float) -> tuple[float, float]:
        """
        Converts normalized grid coordinates (0-16 for x, 0-9 for y)
        to pixel coordinates based on the current viewport size.
        """
        if not self.page:
            logger.error("Page is not available to get click coordinates.")
            return (None, None)

        viewport_size = self.page.viewport_size
        if not viewport_size:
            logger.error("Could not get viewport size.")
            return (None, None)

        viewport_width = viewport_size['width']
        viewport_height = viewport_size['height']

        target_aspect_ratio = 16 / 9
        viewport_aspect_ratio = viewport_width / viewport_height

        rect_width = viewport_width
        rect_height = viewport_height
        offset_x = 0
        offset_y = 0

        # Determine the dimensions of the 16:9 inscribed rectangle
        if viewport_aspect_ratio > target_aspect_ratio:
            # Viewport is wider than 16:9 (letterboxed)
            rect_width = viewport_height * target_aspect_ratio
            offset_x = (viewport_width - rect_width) / 2
        else:
            # Viewport is taller than 16:9 (pillarboxed)
            rect_height = viewport_width / target_aspect_ratio
            offset_y = (viewport_height - rect_height) / 2

        # Normalize grid coordinates (0-16 for x, 0-9 for y)
        if not (0 <= x <= 16 and 0 <= y <= 9):
            logger.warning(f"Click coordinates ({x}, {y}) are outside the 0-16, 0-9 grid.")
            return (None, None)
        
        # Calculate the absolute pixel coordinates
        click_x = offset_x + (x / 16) * rect_width
        click_y = offset_y + (y / 9) * rect_height
        return (click_x, click_y)

    def _move_mouse(self, click_x: float, click_y: float):
        """
        Moves the mouse to the specified pixel coordinates.
        This is a helper function for debugging purposes.
        """
        if not self.page:
            logger.error("Page is not available to move mouse.")
            return

        try:
            logger.info(f"Moving mouse to pixel ({click_x:.2f}, {click_y:.2f})")
            self.page.mouse.move(click_x, click_y)

        except Exception as e:
            logger.error(f"Failed to move mouse: {e}")

    def _click(self, click_x: float, click_y: float):
        """
        Calculates pixel coordinates from a 16x9 grid and clicks there.
        It handles different browser aspect ratios by centering a 16:9
        inscribed rectangle within the viewport.
        """
        if not self.page:
            logger.error("Page is not available to click.")
            return

        try:
            logger.info(f"Clicking at pixel ({click_x:.2f}, {click_y:.2f})")
            self.page.mouse.click(click_x, click_y)

        except Exception as e:
            logger.error(f"Failed to perform click: {e}")


    def _process_commands(self):
        """The main loop to process commands from the queue."""
        while not self._stop_event.is_set():
            try:
                # Wait for a command, with a timeout to allow checking the stop event
                command_data = self.command_queue.get_nowait()

                command = command_data.get("command")
                if command == "click":
                    point = command_data.get("point")
                    if point and len(point) == 2:
                        click_x, click_y = self._get_clickxy(point[0], point[1])
                        if click_x is None or click_y is None:
                            logger.error(f"Invalid click coordinates: {point}")
                            continue
                        self._move_mouse(click_x, click_y)  # Optional: move mouse for debugging
                        self.page.wait_for_timeout(100)  # Wait for a short time to ensure mouse move is registered
                        logger.info(f"Clicking at normalized grid point {point} -> pixel ({click_x:.2f}, {click_y:.2f})")
                        self._click(click_x, click_y)
                    else:
                        logger.error(f"Invalid 'click' command data: {command_data}")
                elif command == "stop":
                    # Exit loop on stop command
                    break
                # The 'delay' command must also use the non-blocking wait.
                elif command == "delay":
                    delay = command_data.get("delay", 0)
                    if isinstance(delay, (int, float)) and delay >= 0:
                        logger.info(f"Delaying for {delay} seconds.")
                        if self.page:
                            self.page.wait_for_timeout(delay * 1000)
                    else:
                        logger.error(f"Invalid 'delay' command data: {command_data}")
                else:
                    logger.warning(f"Unknown command received: {command}")

            except queue.Empty:
                # Queue was empty, loop continues to check the stop event
                if self.page:
                    self.page.wait_for_timeout(20) # Use a small timeout (in ms)
                continue
            except Exception as e:
                logger.error(f"An error occurred in the command processing loop: {e}")


    def start(self):
        """
        Starts the Playwright instance, opens the browser, and begins
        the command processing loop. This method should be the target
        of a thread.
        """
        logger.info("Controller Starting...")
        self.running = True
        self._stop_event.clear()

        try:
            with sync_playwright() as p:
                self.playwright = p
                self.browser = self.playwright.chromium.launch_persistent_context(
                    user_data_dir=Path().cwd() / "playwright_data",
                    headless=False,
                    ignore_default_args=['--enable-automation'],
                    args=["--noerrdialogs"],
                )
                # List all pages in the browser context
                pages: list[Page] = self.browser.pages
                if not pages:
                    logger.error("No pages found in the browser context.")
                    return
                if len(pages) > 1:
                    for page in pages[1:]:
                        logger.info(f"Closing extra page: {page.url}")
                        page.close()
                self.page = pages[0]

                # Set up the WebSocket event listener
                self.page.on("websocket", self._on_web_socket)

                logger.info(f"Navigating to {self.url}...")
                self.page.goto(self.url)
                logger.info("Page loaded. Ready for commands.")


                # Start processing commands
                self._process_commands()

        except Exception as e:
            logger.error(f"A critical error occurred during Playwright startup or operation: {e}")
        finally:
            logger.info("Shutting down...")
            if self.browser:
                self.browser.close()
            self.running = False
            logger.info("Controller Stopped.")

    def stop(self):
        """
        Signals the controller to stop and cleans up resources.
        This method is thread-safe.
        """
        if self.running:
            logger.info("Sending stop signal...")
            self._stop_event.set()
            # Add a stop command to unblock the queue.get() if it's waiting
            self.command_queue.put({"command": "stop"})
        else:
            logger.info("Controller already stopped.")

    def click(self, x: float, y: float):
        """
        Public method to queue a click command. This is thread-safe.

        Args:
            x (float): The x-coordinate on the 16-unit wide grid.
            y (float): The y-coordinate on the 9-unit high grid.
        """
        if self.running:
            command = {"command": "click", "point": [x, y]}
            self.command_queue.put(command)
        else:
            logger.warning("Controller is not running. Cannot queue click command.")
