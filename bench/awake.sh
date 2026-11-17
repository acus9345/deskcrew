#!/bin/bash
# While a visible run goes on with the person away, end the screensaver whenever it starts: real
# input cannot reach the desktop under it. Settings are not touched.
while true; do
  taskkill //IM scrnsave.scr //F >/dev/null 2>&1
  sleep 2
done
