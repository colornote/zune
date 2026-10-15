#!/usr/bin/env python3
"""隔离测试:MTP OpenSession 后 SetDevicePropValue(0xD406) 为何无响应。"""
import struct, time
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
tx=0
def build(typ,code,params=(),t=None):
    b=b"".join(struct.pack("<I",p) for p in params)
    return struct.pack("<IHHI",12+len(b),typ,code,tx if t is None else t)+b
def wr(d): dev.write(OUT,d,5000)
def rd(n=512,to=6000):
    try: return bytes(dev.read(IN,n,timeout=to))
    except usb.core.USBTimeoutError: return None
def recv():
    h=rd()
    if h is None: return None
    total=struct.unpack_from("<I",h)[0]
    buf=h
    while len(buf)<total:
        c=rd(total-len(buf)); 
        if c is None: break
        buf+=c
    return struct.unpack_from("<HH",buf,4), buf

# 1) OpenSession
print(">> OpenSession")
wr(build(1,0x1002,(1,),0)); tx=0
print("   <<", recv())

# 2) SetDevicePropValue 0xD406  (zune-cli 的两段式 DATA 写)
print(">> SetDevicePropValue 0xD406 (2-write data)")
s="libmtp/Sajid Anwar - MTPZClassDriver"
enc=struct.pack("<B",len(s)+1)+(s+"\0").encode("utf-16-le")
tx+=1
wr(build(1,0x1016,(0xD406,)))
wr(struct.pack("<IHHI",12+len(enc),2,0x1016,tx))   # data header
wr(enc)                                            # payload
print("   <<", recv())

# 3) 变体:单段写 DATA
print(">> retry SetDevicePropValue (single-write data)")
tx+=1
wr(build(1,0x1016,(0xD406,)))
wr(struct.pack("<IHHI",12+len(enc),2,0x1016,tx)+enc)
print("   <<", recv())

# 4) 变体:先 GetDeviceInfo 看设备是否还活跃
print(">> GetDeviceInfo (设备是否还活着)")
tx+=1
wr(build(1,0x1001))
print("   <<", recv(512,6000))
