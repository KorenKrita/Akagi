import threading
import traceback
import asyncio
import queue
import time
import mitmproxy.http
import mitmproxy.log
import mitmproxy.tcp
import mitmproxy.websocket
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

    def _click_at_grid_point(self, x: float, y: float):
        """
        Calculates pixel coordinates from a 16x9 grid and clicks there.
        It handles different browser aspect ratios by centering a 16:9
        inscribed rectangle within the viewport.
        """
        if not self.page:
            logger.error("Page is not available to click.")
            return

        try:
            viewport_size = self.page.viewport_size
            if not viewport_size:
                logger.error("Could not get viewport size.")
                return

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

            # Calculate the absolute pixel coordinates
            click_x = offset_x + (x / 16) * rect_width
            click_y = offset_y + (y / 9) * rect_height

            logger.info(f"Clicking at grid ({x}, {y}) -> pixel ({click_x:.2f}, {click_y:.2f})")
            self.page.mouse.click(click_x, click_y)

        except Exception as e:
            logger.error(f"Failed to perform click: {e}")


    def _process_commands(self):
        """The main loop to process commands from the queue."""
        while not self._stop_event.is_set():
            try:
                # Wait for a command, with a timeout to allow checking the stop event
                command_data = self.command_queue.get(timeout=0.1)

                command = command_data.get("command")
                if command == "click":
                    point = command_data.get("point")
                    if point and len(point) == 2:
                        self._click_at_grid_point(point[0], point[1])
                    else:
                        logger.error(f"Invalid 'click' command data: {command_data}")
                elif command == "stop":
                    # Exit loop on stop command
                    break
                elif command == "delay":
                    delay = command_data.get("delay", 0)
                    if isinstance(delay, (int, float)) and delay >= 0:
                        logger.info(f"Delaying for {delay} seconds.")
                        time.sleep(delay)
                    else:
                        logger.error(f"Invalid 'delay' command data: {command_data}")
                else:
                    logger.warning(f"Unknown command received: {command}")

            except queue.Empty:
                # Queue was empty, loop continues to check the stop event
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
                self.browser = self.playwright.chromium.launch(headless=False)
                self.page = self.browser.new_page()

                # Set up the WebSocket event listener
                self.page.on("websocket", self._on_web_socket)

                logger.info(f"Navigating to {self.url}...")
                self.page.goto(self.url, wait_until="domcontentloaded")
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



class ClientWebSocket(ClientWebSocketABC):
    def __init__(self):
        self.bridge_lock = threading.Lock()
        pass

    def websocket_start(self, flow: mitmproxy.http.HTTPFlow):
        assert isinstance(flow.websocket, mitmproxy.websocket.WebSocketData)
        global activated_flows, majsoul_bridges
        logger.info(f"WebSocket connection opened: {flow.id}")
        
        activated_flows.append(flow.id)
        majsoul_bridges[flow.id] = MajsoulBridge()

    def websocket_message(self, flow: mitmproxy.http.HTTPFlow):
        assert isinstance(flow.websocket, mitmproxy.websocket.WebSocketData)
        global activated_flows, majsoul_bridges
        try:
            if flow.id in activated_flows:
                msg = flow.websocket.messages[-1]
                if msg.from_client:
                    logger.debug(f"<- Message: {msg.content}")
                else: # from server
                    logger.debug(f"-> Message: {msg.content}")
                self.bridge_lock.acquire()
                bridge = majsoul_bridges[flow.id]
                msgs = bridge.parse(msg.content)
                self.bridge_lock.release()
                if msgs is None:
                    return
                for m in msgs:
                    mjai_messages.put(m)
            else:
                logger.error(f"WebSocket message received from unactivated flow: {flow.id}")
        except Exception as e:
            # Release the lock if it is locked
            if self.bridge_lock.locked():
                self.bridge_lock.release()
            logger.error(f"Error: {traceback.format_exc()}")
            logger.error(f"Error: {str(e)}")
            logger.error(f"Error: {e.__traceback__.tb_lineno}")

    def websocket_end(self, flow: mitmproxy.http.HTTPFlow):
        global activated_flows, majsoul_bridges
        if flow.id in activated_flows:
            logger.info(f"WebSocket connection closed: {flow.id}")
            activated_flows.remove(flow.id)
            del majsoul_bridges[flow.id]
        else:
            logger.error(f"WebSocket connection closed from unactivated flow: {flow.id}")

async def start_proxy(host, port):
    opts = options.Options(listen_host=host, listen_port=port)
    master = DumpMaster(
        opts,
        with_termlog=False,
        with_dumper=False,
    )
    master.addons.add(ClientWebSocket())
    logger.info(f"Starting MITM proxy server at {host}:{port}")
    await master.run()
    logger.info("MITM proxy server stopped")
    return master

def stop_proxy():
    ctx.master.shutdown()
