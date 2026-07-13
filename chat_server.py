import asyncio
import websockets
import threading

import websockets.server


async def chat(websocket, path):
    # 注册新用户，并将用户加入到活跃用户列表
    user_id = f"user_{len(users)}"
    print(f"New user: {user_id}")
    await notify_users(f"{user_id} has joined the chat.")
    users[websocket] = user_id

    try:
        async for message in websocket:
            print(f"Received message: {message}")
            # 广播消息给所有用户
            await notify_users(f"{user_id}: {message}")
    except websockets.exceptions.ConnectionClosed as e:
        print(f"Connection closed: {e}")
    finally:
        # 用户离开，从活跃用户列表中移除
        del users[websocket]
        await notify_users(f"{user_id} has left the chat.")


async def notify_users(message):
    # 广播消息给所有连接的客户端
    for user in users:
        print(f"Send message to {users[user]}")
        await user.send(message)


# 存储所有活跃的WebSocket连接
users = {}


async def main():
    async with websockets.server.serve(chat, "0.0.0.0", 8765):
        await asyncio.Future()  # run forever


def start_server():
    asyncio.run(main())


server_thread = threading.Thread(target=start_server)
server_thread.start()
