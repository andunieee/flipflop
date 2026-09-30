#!/usr/bin/env python3
"""Backport blew's Android central code to Android < 13 (API 33).

blew targets API 33+: it calls BluetoothGatt.writeCharacteristic(char, value, type) and
writeDescriptor(descriptor, value), which do not exist before 13, and only overrides the 13+
GATT callback signatures, so on older systems reads and notifications are never delivered.
Usage: patch-blew.py <dir containing GattConnections.kt>
"""
import re, sys

path = sys.argv[1].rstrip("/") + "/GattConnections.kt"
src = open(path).read()

def sub(pattern, repl, count, flags=0):
    global src
    src, n = re.subn(pattern, repl, src, flags=flags)
    assert n == count, f"expected {count} match(es) for {pattern!r}, got {n} (blew changed?)"

# 1. writeCharacteristic(char, value, type)
sub(r"val ret = gatt\.writeCharacteristic\(char, value, writeType\)",
    "val ret = writeCharacteristicCompat(gatt, char, value, writeType)", 1)

# 2. writeDescriptor(descriptor, value) in subscribe and unsubscribe
sub(r"gatt\.writeDescriptor\(\s*descriptor,\s*(BluetoothGattDescriptor\.(?:ENABLE|DISABLE)_NOTIFICATION_VALUE),?\s*\)",
    r"writeDescriptorCompat(gatt, descriptor, \1)", 2)

# 3. legacy callback signatures, delegating to the 13+ ones
anchor = ("            override fun onCharacteristicRead(\n"
          "                gatt: BluetoothGatt,\n"
          "                characteristic: BluetoothGattCharacteristic,\n"
          "                value: ByteArray,")
legacy = '''            // Android < 13 only calls these older signatures.
            @Suppress("DEPRECATION", "OVERRIDE_DEPRECATION")
            override fun onCharacteristicRead(
                gatt: BluetoothGatt,
                characteristic: BluetoothGattCharacteristic,
                status: Int,
            ) {
                onCharacteristicRead(gatt, characteristic, characteristic.value ?: ByteArray(0), status)
            }

            @Suppress("DEPRECATION", "OVERRIDE_DEPRECATION")
            override fun onCharacteristicChanged(
                gatt: BluetoothGatt,
                characteristic: BluetoothGattCharacteristic,
            ) {
                onCharacteristicChanged(gatt, characteristic, characteristic.value ?: ByteArray(0))
            }

'''
assert src.count(anchor) == 1, "callback anchor not found (blew changed?)"
src = src.replace(anchor, legacy + anchor)

src += '''

// ---- backports for Android < 13 (added by android/patch-blew.py) ----

@Suppress("DEPRECATION")
private fun writeCharacteristicCompat(
    gatt: BluetoothGatt,
    char: BluetoothGattCharacteristic,
    value: ByteArray,
    writeType: Int,
): Int =
    if (android.os.Build.VERSION.SDK_INT >= 33) {
        gatt.writeCharacteristic(char, value, writeType)
    } else {
        char.writeType = writeType
        char.value = value
        if (gatt.writeCharacteristic(char)) 0 else -1
    }

@Suppress("DEPRECATION")
private fun writeDescriptorCompat(
    gatt: BluetoothGatt,
    descriptor: BluetoothGattDescriptor,
    value: ByteArray,
): Int =
    if (android.os.Build.VERSION.SDK_INT >= 33) {
        gatt.writeDescriptor(descriptor, value)
    } else {
        descriptor.value = value
        if (gatt.writeDescriptor(descriptor)) 0 else -1
    }
'''
open(path, "w").write(src)
