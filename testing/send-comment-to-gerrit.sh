#!/bin/bash -ex
# This script expects the setup realized in prepopulate.nohup
# namely: a review by admin, with id=1 and jdoe user
ssh -o UserKnownHostsFile=/dev/null \
    -o StrictHostKeyChecking=no \
    -i ./id_rsa_jdoe \
    -p 29418 \
    jdoe@localhost \
    gerrit review ${1:-1,1} -m "$(shuf -n5 /usr/share/dict/american-english)"