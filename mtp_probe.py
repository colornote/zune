#!/usr/bin/env python3
import sys, struct, time
import usb.core, usb.util, usb.backend.libusb1 as l1
LIB="~/local/lib/libusb-1.0.dylib"; EP_IN,EP_OUT,EP_INT=0x81,0x02,0x83
be=l1.get_backend(find_library=lambda x: LIB)
dev=usb.core.find(idVendor=0x045E,idProduct=0x0710,backend=be)
dev.set_configuration()
try:
    if dev.is_kernel_driver_active(0): dev.detach_kernel_driver(0)
except Exception: pass
usb.util.claim_interface(dev,0)
tid=0
def send(code,params=()):
    global tid; tid+=1
    pkt=struct.pack('<IHHI',12+4*len(params),1,code,tid)+b''.join(struct.pack('<I',p) for p in params)
    dev.write(EP_OUT,pkt,timeout=4000); print(f"  >> op 0x{code:04x} tid={tid} params={[hex(p) for p in params]}")
def drain(label,timeout=5000):
    # 读 bulk IN 直到超时
    got=b''
    while True:
        try: c=dev.read(EP_IN,64,timeout=timeout)
        except usb.core.USBTimeoutError: break
        if not c: break
        got+=bytes(c)
        if len(c)<64: break
    print(f"  << {label} bulk_in {len(got)}B: {got[:64].hex()}")
    # 读中断端点
    try:
        i=dev.read(EP_INT,8,timeout=1000); print(f"  << int_in: {bytes(i).hex()}")
    except Exception as e: pass
    return got

print("=== OpenSession ===")
send(0x1002,[1]); r=drain("opensession")
print("=== GetDeviceInfo ===")
send(0x1001); drain("getdevinfo",timeout=8000)
print("=== GetStorageIDs ===")
send(0x1004); drain("getstorageids",timeout=8000)
print("=== 尝试 Zune 扩展: GetWifiState? cmd? 试探 0x9210 ===")
send(0x9210); drain("0x9210",timeout=3000)
