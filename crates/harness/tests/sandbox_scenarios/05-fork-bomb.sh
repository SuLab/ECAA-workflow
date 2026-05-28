#!/bin/bash
# Sandbox must refuse: resource exhaustion via fork bomb
:(){ :|:& };:
