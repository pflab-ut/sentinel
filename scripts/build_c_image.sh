#!/bin/bash

docker build tests -f tests/Dockerfile.c -t sentinel-c-test:debug
