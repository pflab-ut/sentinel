from os import terminal_size
import socket
import sys

if len(sys.argv) != 2:
    print(f'Usage: python {sys.argv[0]} <port number>')
    exit(1)

server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
server.bind((sys.argv[1], 8080))
server.listen(1)
client, address = server.accept()
print('Connection from', address)

while True:
  data = client.recv(1024)
  str_data = data.decode()
  if not data:
      break
  print('Received from client', str_data, end='')
  client.send(data)

client.close()
