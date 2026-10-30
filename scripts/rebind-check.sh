#!/bin/sh
# rebind-check.sh [ROUNDS] [GAP] [HID_ID]: unbind a DualSense from hid-playstation and bind it back
# ROUNDS times (default 20), GAP seconds apart (default 0.05), as a driver switch does: its hidraw
# and event nodes leave and return under the same names. After each round InputPlumber must have
# one DualSense composite device that holds the pad's hidraw node. Stops at the first round that
# breaks. HID_ID defaults to the first DualSense with a physical path (a real pad or
# dualsense-gadget.py), which skips InputPlumber's own virtual DualSense.
rounds=${1:-20}
gap=${2:-0.05}
id=${3:-}
drv=/sys/bus/hid/drivers/playstation
ip=org.shadowblip.InputPlumber
cd_iface=org.shadowblip.Input.CompositeDevice

if [ -z "$id" ]; then
  for x in $(ls "$drv" | grep -i '^000[35]:054C:0CE6'); do
    grep -q '^HID_PHYS=.' "/sys/bus/hid/devices/$x/uevent" && id=$x && break
  done
fi
[ -n "$id" ] || { echo "No DualSense is bound to hid-playstation." >&2; exit 1; }

check() {
  found=0
  sources=""
  for c in $(busctl --system tree $ip | grep -oE '/org/shadowblip/InputPlumber/CompositeDevice[0-9]+$' | sort -u); do
    busctl --system get-property $ip "$c" $cd_iface Name | grep -q DualSense || continue
    found=$((found + 1))
    sources="${c##*/}:$(busctl --system get-property $ip "$c" $cd_iface SourceDevicePaths | grep -oE '(hidraw|event)[0-9]+' | tr '\n' ' ')"
  done
  [ "$found" -eq 1 ] && echo "$sources" | grep -q hidraw
}

i=0
while [ "$i" -lt "$rounds" ]; do
  i=$((i + 1))
  printf '%s' "$id" | sudo tee "$drv/unbind" >/dev/null
  sleep "$gap"
  printf '%s' "$id" | sudo tee "$drv/bind" >/dev/null
  sleep 4
  if check; then
    echo "round $i: ok ($sources)"
  else
    echo "round $i: BROKEN ($found DualSense composite devices; last: ${sources:-none})"
    exit 1
  fi
done
echo "All $rounds rounds kept the pad managed."
