import websocket
import signal
import sys

ws = websocket.create_connection("ws://127.0.0.1:3000/ws")
ws.send("queue length")


def handler(signum, frame):
    print("\nClosing WS connection...")
    # timeout=0 car il reçoit pas le closing frame ???
    ws.close(websocket.STATUS_GOING_AWAY, timeout=0)
    sys.exit()


signal.signal(signal.SIGINT, handler)

while True:
    queue_length = int.from_bytes(ws.recv(), "big")
    print(queue_length)
