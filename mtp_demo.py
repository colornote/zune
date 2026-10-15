#!/usr/bin/env python3
"""Minimal MTP client over libusb/pyusb — 读 Zune 里的对象数量(音乐等)。

USB: VID 0x045e PID 0x0710 (Microsoft Zune)
EP:  0x81 Bulk IN / 0x02 Bulk OUT / 0x83 Interrupt IN
"""
import sys, struct, time
import usb.core, usb.util, usb.backend.libusb1 as l1

LIB = "~/local/lib/libusb-1.0.dylib"
VID, PID = 0x045E, 0x0710

EP_IN, EP_OUT, EP_INT = 0x81, 0x02, 0x83
MAXPKT = 64

# MTP operation codes
GET_DEVICE_INFO   = 0x1001
OPEN_SESSION      = 0x1002
CLOSE_SESSION     = 0x1003
GET_STORAGE_IDS   = 0x1004
GET_STORAGE_INFO  = 0x1005
GET_NUM_OBJECTS   = 0x1006
GET_OBJECT_HANDLES= 0x1007
GET_OBJECT_INFO   = 0x1008

CT_OP=1; CT_DATA=2; CT_RESP=3

class Mtp:
    def __init__(self, dev):
        self.dev = dev
        self.tid = 0
        self.buf = bytearray()
        self.maxpkt_in = 64

    # --- USB helpers ---
    def _read_ep(self, size=64, timeout=4000):
        return bytes(self.dev.read(EP_IN, size, timeout=timeout))

    def _fill(self, n):
        while len(self.buf) < n:
            chunk = self._read_ep(MAXPKT)
            if not chunk:
                break
            self.buf += chunk

    def read_container(self, timeout=4000):
        # header
        self._fill(12)
        if len(self.buf) < 12:
            return None
        length, ctype, code, tid = struct.unpack_from('<IHHI', self.buf, 0)
        if ctype == CT_DATA and length == 0xFFFFFFFF:
            # streaming: read until short packet
            while True:
                chunk = self._read_ep(MAXPKT, timeout)
                self.buf += chunk
                if len(chunk) < MAXPKT:
                    break
            payload = bytes(self.buf[12:])
            self.buf = bytearray()
            return (ctype, code, tid, payload)
        # fixed length
        self._fill(length)
        c = bytes(self.buf[:length])
        del self.buf[:length]
        payload = c[12:]
        return (ctype, code, tid, payload)

    def send_op(self, code, params=(), data=None):
        self.tid += 1
        tid = self.tid
        n = len(params)
        pkt = struct.pack('<IHHI', 12 + 4*n, CT_OP, code, tid) + b''.join(struct.pack('<I', p) for p in params)
        self.dev.write(EP_OUT, pkt, timeout=4000)
        if data is not None:
            dpkt = struct.pack('<IHHI', 12 + len(data), CT_DATA, code, tid) + data
            self.dev.write(EP_OUT, dpkt, timeout=4000)
        return tid

    def read_resp(self):
        """读一个响应(跳过可能出现的 Data 容器)。"""
        while True:
            c = self.read_container()
            if c is None:
                return None
            ctype, code, tid, payload = c
            if ctype == CT_RESP:
                params = [struct.unpack_from('<I', payload, i)[0] for i in range(0, len(payload), 4)]
                return (code, params)
            # DATA -> 继续读响应

    def op(self, code, params=(), expect_data=False):
        self.send_op(code, params)
        data = None
        if expect_data:
            c = self.read_container()
            if c and c[0] == CT_DATA:
                data = c[3]
        resp = self.read_resp() if not expect_data else self.read_resp()
        return data, resp

# ---- MTP dataset parsing ----
def mtp_string(b, off):
    ln = b[off]; off += 1
    s = b[off:off+ln*2].decode('utf-16le', 'replace').rstrip('\x00')
    return s, off+ln*2

def parse_device_info(d):
    off = 0
    std, vend_id = struct.unpack_from('<HI', d, off); off += 6
    vend_ver, = struct.unpack_from('<H', d, off); off += 2
    vend_desc, off = mtp_string(d, off)
    funcmode, = struct.unpack_from('<H', d, off); off += 2
    def arr(off):
        n, = struct.unpack_from('<I', d, off); off += 4
        return [struct.unpack_from('<H', d, off+2*i)[0] for i in range(n)], off+2*n
    ops, off = arr(off)
    evts, off = arr(off)
    props, off = arr(off)
    capf, off = arr(off)
    imgf, off = arr(off)
    manu, off = mtp_string(d, off)
    model, off = mtp_string(d, off)
    ver, off = mtp_string(d, off)
    serial, off = mtp_string(d, off)
    return dict(stdver=std, vend_id=vend_id, vend_desc=vend_desc, funcmode=funcmode,
                ops=ops, manu=manu, model=model, ver=ver, serial=serial)

def parse_storage_info(d):
    off = 0
    sid,=struct.unpack_from('<H',d,off); off+=2
    typ,=struct.unpack_from('<H',d,off); off+=2
    fs,=struct.unpack_from('<H',d,off); off+=2
    acc,=struct.unpack_from('<H',d,off); off+=2
    cap,free,=struct.unpack_from('<QQ',d,off); off+=16
    desc,off=mtp_string(d,off)
    vol,off=mtp_string(d,off)
    return dict(type=typ, cap=cap, free=free, desc=desc, vol=vol)

def main():
    be = l1.get_backend(find_library=lambda x: LIB)
    dev = usb.core.find(idVendor=VID, idProduct=PID, backend=be)
    if not dev:
        print("找不到 Zune"); return
    dev.set_configuration()
    try:
        if dev.is_kernel_driver_active(0):
            dev.detach_kernel_driver(0)
    except Exception:
        pass
    usb.util.claim_interface(dev, 0)
    m = Mtp(dev)

    # OpenSession
    m.send_op(OPEN_SESSION, [1])
    r = m.read_resp()
    print("OpenSession ->", r)

    # GetDeviceInfo
    data, resp = m.op(GET_DEVICE_INFO, expect_data=True)
    if data:
        di = parse_device_info(data)
        print(f"设备: {di['manu']} {di['model']}  ver={di['ver']}  sn={di['serial']}")
        print(f"  支持 {len(di['ops'])} 个操作码")
    print("GetDeviceInfo resp:", resp)

    # GetStorageIDs
    data, resp = m.op(GET_STORAGE_IDS, expect_data=True)
    ids = []
    if data:
        n = struct.unpack_from('<I', data, 0)[0]
        ids = [struct.unpack_from('<I', data, 4+4*i)[0] for i in range(n)]
    print("StorageIDs:", [hex(x) for x in ids], "resp:", resp)

    total = 0
    for sid in ids:
        d, r = m.op(GET_STORAGE_INFO, [sid], expect_data=True)
        if d:
            si = parse_storage_info(d)
            print(f"  storage 0x{sid:x}: type={si['type']} cap={si['cap']} free={si['free']} '{si['desc']}' '{si['vol']}'")
        # GetNumObjects(storage, format=0(all), parent=0xFFFFFFFF)
        d, rm = m.op(GET_NUM_OBJECTS, [sid, 0x00000000, 0xFFFFFFFF])
        print(f"  GetNumObjects(storage=0x{sid:x}) resp={rm}")
        # 尝试列出所有对象句柄计数
        d, rh = m.op(GET_OBJECT_HANDLES, [sid, 0x00000000, 0xFFFFFFFF], expect_data=True)
        if d:
            n = struct.unpack_from('<I', d, 0)[0]
            print(f"  GetObjectHandles(ALL) -> {n} 个对象")
            total += n
    print(f"\n合计对象数: {total}")

    m.send_op(CLOSE_SESSION)
    m.read_resp()

if __name__ == '__main__':
    main()
