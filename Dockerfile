FROM rust

RUN apt-get update
RUN apt-get -y install apt-transport-https ca-certificates curl gnupg2 software-properties-common
RUN curl -fsSL https://download.docker.com/linux/debian/gpg | apt-key add -
RUN add-apt-repository "deb [arch=amd64] https://download.docker.com/linux/debian $(lsb_release -cs) stable"
RUN apt-get install -y build-essential git clang cmake libstdc++-10-dev libssl-dev libxxhash-dev zlib1g-dev pkg-config

# use mold!
ENV MOLD_PATH=/home/mold
RUN git clone https://github.com/rui314/mold.git $MOLD_PATH
WORKDIR ${MOLD_PATH}
RUN git checkout v1.0.2
RUN make -j$(nproc) CXX=clang++
RUN make install

RUN apt-get update
RUN apt-get -y install docker-ce
RUN rustup install nightly

# setup project
ENV SENTINEL_PATH=/home/sentinel
WORKDIR ${SENTINEL_PATH}
COPY Cargo.lock .
COPY Cargo.toml .
COPY ./src src
COPY ./resources resources
RUN mkdir .cargo
RUN cargo vendor > .cargo/config

CMD ["cargo", "+nightly", "run", "--", "-d", "ubuntu-echo", "-e", "/echo"]
