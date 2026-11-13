#!/bin/sh
# Runs the platform layer's instrumented tests (PlatformDeviceTest) on a running emulator, and
# plays the host's part in the two that need one:
#   - geofenceFiresOnMockLocation: holds the emulator's GPS inside the test geofence (adb emu geo fix)
#   - unifiedPushWakesTheCore: POSTs a content-free push to the endpoint the test logs. Needs a
#     UnifiedPush distributor (ntfy) on the emulator whose default server is a local ntfy, e.g.
#     `ntfy serve --listen-http 127.0.0.1:8099 --base-url http://10.0.2.2:8099`.
#
# Usage: ANDROID_SERIAL=emulator-5562 ./platform-tests.sh [gms|foss]
# The APKs must be installed first: ./gradlew install<Flavor>Debug install<Flavor>DebugAndroidTest
set -eu
: "${ANDROID_SERIAL:?set ANDROID_SERIAL to the emulator's serial}"
flavor="${1:-gms}"
adb="adb -s $ANDROID_SERIAL"
runner=org.pingers.app.debug.test/androidx.test.runner.AndroidJUnitRunner
class=org.pingers.app.platform.PlatformDeviceTest
out=$(mktemp)

run() { $adb shell am instrument -w -e class "$class#$1" "$runner"; }

for t in secureStoreWrapsWithTheKeystore interruptionFilterChangeIsSeen foregroundServiceStartsAndStops fcmTokenIsObtained; do
    echo "== $t"; run "$t" | tail -3
done

echo "== geofenceFiresOnMockLocation ($flavor)"
$adb emu geo fix -0.2000 51.4000 >/dev/null
run geofenceFiresOnMockLocation >"$out" 2>&1 &
pid=$!
sleep 8
while kill -0 "$pid" 2>/dev/null; do
    $adb emu geo fix -0.1246 51.5007 >/dev/null
    sleep 2
done
tail -3 "$out"

echo "== unifiedPushWakesTheCore"
$adb logcat -c
run unifiedPushWakesTheCore >"$out" 2>&1 &
pid=$!
endpoint=""
while kill -0 "$pid" 2>/dev/null && [ -z "$endpoint" ]; do
    endpoint=$($adb logcat -d -s PingersTest | sed -n 's/.*PINGERS_ENDPOINT \([^ ]*\).*/\1/p' | head -1 | tr -d '\r')
    sleep 1
done
if [ -n "$endpoint" ]; then
    sleep 3
    curl -fsS -X POST -d '{"v":1,"h":"AQIDBAUGBwg="}' "$(echo "$endpoint" | sed 's/10\.0\.2\.2/127.0.0.1/')" >/dev/null
fi
wait "$pid" || true
tail -3 "$out"
rm -f "$out"
