FROM ubuntu
RUN apt update
RUN apt install -y build-essential
COPY ./app/c/* /home/
RUN gcc -o /home/echo /home/echo.c
RUN gcc -o /home/hello_world /home/hello_world.c
RUN gcc -o /home/open /home/open.c
CMD ["bash"]
