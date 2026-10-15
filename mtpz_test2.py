#!/usr/bin/env python3
"""干净状态单测:OpenSession -> SetDevicePropValue(0xD406) 单段写。"""
import struct
import usb.core, usb.util, usb.backend.libusb1 as l1
LIB="/usr/local/lib/libusb-1.0.dylib"
be=l1.get_backend(find_library=lambda x:LIB)
dev=usb.core.find(idVendor=0x045E,idProduct=0x0710,backend=be)
dev.set_configuration()
try:
    if dev.is_kernel_driver_active(0): dev.detach_kernel_driver(0)
except Exception: pass
usb.util.claim_interface(dev,0)
IN,OUT=0x81,0x02
def build(typ,code,tx,params=()):
    b=b"".join(struct.pack("<I",p) for p in params)
    return struct.pack("<IHHI",12+len(b),typ,code,tx)+b
def wr(d): dev.write(OUT,d,5000)
def rd(n=512,to=8000):
    try: return bytes(dev.read(IN,n,timeout=to))
    except usb.core.USBTimeoutError: return None
def recv(to=8000):
    h=rd(512,to)
    if h is None: return None
    total=struct.unpack_from("<I",h)[0]
    buf=h
    while len(buf)<total:
        c=rd(total-len(buf),to)
        if c is None: break
        buf+=c
    return struct.unpack_from("<HH",buf,4), struct.unpack_from("<I",buf,8)[0], buf[12:total]

print(">> OpenSession")
wr(build(1,0x1002,0,(1,)))
print("   <<", recv())

s="libmtp/Sajid Anwar - MTPZClassDriver"
enc=struct.pack("<B",len(s)+1)+(s+"\0").encode("utf-16-le")
print(f"   encoded {len(enc)} bytes")

print(">> SetDevicePropValue 0xD406 —— 变体:cmd+data 各自单段写")
wr(build(1,0x1016,1,(0xD406,)))          # Command
wr(build(2,0x1016,1)+enc)                # Data (single transfer)
print("   <<", recv())
