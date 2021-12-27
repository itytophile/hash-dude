import websocket
import signal
import sys
import os
import math

ws = websocket.create_connection("ws://127.0.0.1:3000/ws")
ws.send("queue length")
print("Successfully connected to server")


def handler(signum, frame):
    print("\nClosing WS connection...")
    # timeout=0 car il reçoit pas le closing frame ???
    ws.close(websocket.STATUS_GOING_AWAY, timeout=0)
    sys.exit()


signal.signal(signal.SIGINT, handler)

REQUIRED_REPLICAS_COUNT_FOR_LENGTH = [
    (2, 1),
    (4, 2),
    (8, 4),
    (math.inf, 8),
]  # (length, count)

replicas_count = 1

while True:
    queue_length = int.from_bytes(ws.recv(), "big")
    print("queue_length =", queue_length)
    for (length, count) in REQUIRED_REPLICAS_COUNT_FOR_LENGTH:
        if queue_length <= length:
            if replicas_count != count:
                print(f"Updating slaves: {replicas_count} ->", count)
                # peux pas utiliser --no-recreate https://github.com/docker/compose/issues/8940
                os.system(f"docker service scale stackhash_slave={count}")
                replicas_count = count
            break
