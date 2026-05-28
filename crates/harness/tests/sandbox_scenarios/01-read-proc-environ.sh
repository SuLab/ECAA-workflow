#!/bin/bash
# Sandbox must refuse: reading process environment (secret exposure)
cat /proc/self/environ
