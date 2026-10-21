#!/bin/sh
# Builds run as the caller's uid (`make in-docker` passes `--user`), which the
# image does not know. Programs that look the user up, such as the tests'
# dbus-daemon, refuse a uid without a passwd entry, so give it one.
uid=$(id -u)
gid=$(id -g)
if ! getent passwd "$uid" >/dev/null; then
  echo "build-$uid:x:$uid:$gid:build:${HOME:-/tmp}:/bin/sh" >>/etc/passwd
fi
if ! getent group "$gid" >/dev/null; then
  echo "build-$gid:x:$gid:" >>/etc/group
fi
exec "$@"
