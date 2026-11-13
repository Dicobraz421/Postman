#!/bin/sh
# Postman (LIN-06): deb postinst/prerm and rpm %post/%preun (scripts/linux/package.sh). $1 - configure (after install
# or upgrade), remove (before removal), anything else - nothing. The AppArmor profile (ADR 0010: user namespaces of the
# worker and the viewer) goes to /etc/apparmor.d only where AppArmor 4 is (abi/4.0; older ones have no userns rule and
# no restriction). A failed load does not fail the package: the worker then runs without namespaces, as the AppImage.
set -e
pm_src=/usr/share/postman/apparmor/postman
pm_dst=/etc/apparmor.d/postman
case "$1" in
  configure)
    if [ -e /etc/apparmor.d/abi/4.0 ] && [ -e "$pm_src" ]; then
      cp "$pm_src" "$pm_dst"
      if command -v apparmor_parser > /dev/null 2>&1 && [ -d /sys/kernel/security/apparmor ]; then
        apparmor_parser -r -W "$pm_dst" || echo "postman: AppArmor profile not loaded, the worker sandbox runs without user namespaces" >&2
      fi
    fi
    ;;
  remove)
    if [ -e "$pm_dst" ]; then
      if command -v apparmor_parser > /dev/null 2>&1 && [ -d /sys/kernel/security/apparmor ]; then
        apparmor_parser -R "$pm_dst" > /dev/null 2>&1 || true
      fi
      rm -f "$pm_dst"
    fi
    ;;
esac
