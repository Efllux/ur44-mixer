#!/bin/zsh
# Build the UR44 driver: the Core Audio plug-in (build/UR44.driver) and the test tools.
# Universal binaries (arm64 + x86_64), macOS 12 or later. Port of QuadCaptureMac's build.sh (MIT).
#   ./build.sh                  build with the default SDK
#   SDK=macosx15.4 ./build.sh   build with another installed SDK
set -euo pipefail
cd "${0:a:h}"
mkdir -p build

VERSION=$(cat VERSION)
SDKPATH=$(xcrun --sdk "${SDK:-macosx}" --show-sdk-path)
ARCH=(-arch arm64 -arch x86_64)
MINOS=12.0
COMMON=(-isysroot "$SDKPATH" $ARCH -mmacosx-version-min=$MINOS -O2 -Wall -Wextra -Wno-unused-parameter
        -Werror=unguarded-availability-new -Werror=partial-availability -Isrc)
OBJC=(-fobjc-arc)
FW=(-framework Foundation -framework IOKit -framework IOUSBHost)

clang $COMMON $OBJC -o build/ur_engine_test tools/ur_engine_test.m src/engine/ur_engine.m $FW
clang $COMMON -o build/ur_hal_test tools/ur_hal_test.c -framework CoreAudio -framework CoreFoundation

DRV=build/UR44.driver
rm -rf $DRV
mkdir -p $DRV/Contents/MacOS
sed "s/@VERSION@/$VERSION/g" src/driver/Info.plist > $DRV/Contents/Info.plist
clang $COMMON -x c -c -o build/ur_driver.o src/driver/ur_driver.c
clang $COMMON $OBJC -c -o build/ur_engine.o src/engine/ur_engine.m
clang $COMMON -bundle -o $DRV/Contents/MacOS/UR44 build/ur_driver.o build/ur_engine.o \
    $FW -framework CoreAudio -framework CoreFoundation
rm -f build/ur_driver.o build/ur_engine.o
codesign --force --sign - --timestamp=none $DRV
echo "Built $DRV $VERSION ($(lipo -archs $DRV/Contents/MacOS/UR44), macOS $MINOS+, SDK $(basename $SDKPATH))"
