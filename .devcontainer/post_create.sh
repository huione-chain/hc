#!/bin/bash

apt update && apt upgrade -y
apt-get purge -y --allow-remove-essential imagemagick imagemagick-6-common
apt install -y --no-install-recommends build-essential cmake libsodium-dev libsecp256k1-dev \
    liblz4-dev libssl-dev zlib1g-dev ccache protobuf-compiler docker.io \
    clang-16 fish vim emacs-nox git

git config --global user.name 'Feng Ge'
git config --global user.email 'rustedLightning@yandex.com'

/usr/local/cargo/bin/cargo install taplo-cli ripgrep fd-find
