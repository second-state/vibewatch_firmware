from http.server import BaseHTTPRequestHandler, HTTPServer
import os


class SimpleHTTPRequestHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        # 检查请求路径是否为 "/upload"
        if self.path == "/upload":
            # 获取内容长度
            print(self.headers["Content-Length"])
            content_length = int(self.headers["Content-Length"])
            # 读取请求体中的数据
            post_data = self.rfile.read(content_length)

            # 将数据写入 data.wav 文件
            with open("data.wav", "wb") as file:
                file.write(post_data)

            # 发送响应
            self.send_response(200)
            self.send_header("Content-type", "text/plain")
            self.end_headers()
            self.wfile.write(b"File uploaded successfully.")
        else:
            # 如果路径不是 "/upload"，返回 404 错误
            self.send_response(404)
            self.send_header("Content-type", "text/plain")
            self.end_headers()
            self.wfile.write(b"Not Found.")


def run(server_class=HTTPServer, handler_class=SimpleHTTPRequestHandler, port=8080):
    server_address = ("", port)
    httpd = server_class(server_address, handler_class)
    print(f"Starting httpd server on port {port}...")
    httpd.serve_forever()


if __name__ == "__main__":
    run()
