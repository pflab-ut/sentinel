#!/bin/bash

docker build tests -f tests/Dockerfile.python -t sentinel-python-test:debug
