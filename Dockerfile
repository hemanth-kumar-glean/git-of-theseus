FROM rust:1

COPY ./container/ /
COPY ./ /got/

RUN apt-get update -q && \
    apt-get install -yqq git && \
    rm -rf /var/lib/apt/lists/* && \
    cargo install --path /got/rust --locked

WORKDIR /got/

CMD ["/bin/bash"]
