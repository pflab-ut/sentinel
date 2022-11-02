#!/bin/bash

docker build tests -f tests/Dockerfile.ruby -t sentinel-ruby-test:debug
