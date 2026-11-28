"""Audited container PID 1 only; never run as a host OCR fallback.

The root monitor only frames input and checks PNG structure/CRC and hashes.
All native image decoding/OCR runs as uid/gid 65534. No caller code executes.
"""
import argparse
import base64
import hashlib
import json
import os
import resource
import selectors
import struct
import subprocess
import time
import zlib


def emit(code, text=b'', error=b'', timed_out=False, output_limited=False, provenance=None):
    packet=(json.dumps({'version':1,'exit_code':code,'text_base64':base64.b64encode(text).decode('ascii'),
        'stderr_base64':base64.b64encode(error[:4096]).decode('ascii'),'timed_out':timed_out,
        'output_limited':output_limited,'provenance':provenance},separators=(',',':'))+'\n').encode('ascii')
    os.set_blocking(1,False)
    until=time.monotonic()+0.5
    with selectors.DefaultSelector() as selector:
        selector.register(1,selectors.EVENT_WRITE)
        try:
            while packet and time.monotonic()<until:
                if selector.select(max(0,until-time.monotonic())):
                    packet=packet[os.write(1,packet):]
        except (BrokenPipeError,BlockingIOError):
            pass
    os._exit(0)


def read_input(deadline,length,digest):
    data=bytearray()
    os.set_blocking(0,False)
    with selectors.DefaultSelector() as selector:
        selector.register(0,selectors.EVENT_READ)
        while True:
            remaining=deadline-time.monotonic()
            if remaining<=0:
                emit(124,timed_out=True)
            if selector.select(remaining):
                block=os.read(0,8192)
                if not block:
                    break
                data.extend(block)
                if len(data)>length:
                    raise ValueError('oversized input')
    if len(data)!=length or hashlib.sha256(data).hexdigest()!=digest:
        raise ValueError('incomplete input frame')
    return bytes(data)


def png(data,max_pixels,deadline):
    if data[:8]!=b'\x89PNG\r\n\x1a\n':
        raise ValueError('PNG required')
    offset=8
    dimensions=None
    saw_idat=False
    ended_idat=False
    while offset<len(data):
        if time.monotonic()>=deadline:
            emit(124,timed_out=True)
        if offset+12>len(data):
            raise ValueError('truncated PNG chunk')
        length=struct.unpack('>I',data[offset:offset+4])[0]
        end=offset+12+length
        if end>len(data):
            raise ValueError('truncated PNG data')
        name=data[offset+4:offset+8]
        content=data[offset+8:end-4]
        if zlib.crc32(data[offset+4:end-4])!=struct.unpack('>I',data[end-4:end])[0]:
            raise ValueError('PNG CRC mismatch')
        if dimensions is None:
            if name!=b'IHDR' or length!=13:
                raise ValueError('PNG IHDR required')
            width,height,depth,color,compression,filtering,interlace=struct.unpack('>IIBBBBB',content)
            allowed={0:(1,2,4,8,16),2:(8,16),3:(1,2,4,8),4:(8,16),6:(8,16)}
            if not 1<=width<=16384 or not 1<=height<=16384 or width*height>max_pixels or depth not in allowed.get(color,()) or compression or filtering or interlace not in (0,1):
                raise ValueError('PNG dimensions/format refused')
            dimensions=(width,height)
        elif name==b'IHDR' or name in (b'acTL',b'fcTL',b'fdAT'):
            raise ValueError('multiple/animated PNG refused')
        elif name==b'IDAT':
            if ended_idat:
                raise ValueError('nonconsecutive PNG data')
            saw_idat=True
        elif name==b'IEND':
            if length or end!=len(data) or not saw_idat:
                raise ValueError('invalid PNG end')
            return dimensions
        else:
            if saw_idat:
                ended_idat=True
            if name[0]&32==0 and name!=b'PLTE':
                raise ValueError('unknown critical PNG chunk')
        offset=end
    raise ValueError('PNG end missing')


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--timeout-ms',type=int,required=True)
    parser.add_argument('--max-output-bytes',type=int,required=True)
    parser.add_argument('--max-text-bytes',type=int,required=True)
    parser.add_argument('--png-bytes',type=int,required=True)
    parser.add_argument('--png-sha256',required=True)
    parser.add_argument('--max-pixels',type=int,required=True)
    parser.add_argument('--language',choices=('eng','chi_sim','chi_sim+eng'),required=True)
    args=parser.parse_args()
    if os.getpid()!=1 or os.getuid()!=0:
        raise RuntimeError('container PID 1 monitor required')
    if not 1<=args.timeout_ms<=30000 or not 1024<=args.max_output_bytes<=65536 or not 1<=args.max_text_bytes<=args.max_output_bytes or not 1<=args.png_bytes<=4*1024*1024 or not 1<=args.max_pixels<=16*1024*1024 or len(args.png_sha256)!=64 or any(c not in '0123456789abcdef' for c in args.png_sha256):
        raise ValueError('invalid fixed limits')
    # Starts before the first stdin read: partial ingress/absent EOF is bounded.
    deadline=time.monotonic()+args.timeout_ms/1000
    data=read_input(deadline,args.png_bytes,args.png_sha256)
    width,height=png(data,args.max_pixels,deadline)
    with open('/opt/taskloom/ocr-manifest.json','rb') as source:
        raw_manifest=source.read(16385)
    if len(raw_manifest)>16384:
        raise ValueError('manifest exceeds bound')
    manifest=json.loads(raw_manifest)
    if manifest.get('version')!=1 or set(manifest.get('traineddata_sha256',{}))!={'eng','chi_sim'}:
        raise ValueError('invalid pinned manifest')
    for name,expected in manifest['traineddata_sha256'].items():
        digest=hashlib.sha256()
        with open('/usr/share/tesseract-ocr/5/tessdata/'+name+'.traineddata','rb') as source:
            while True:
                if time.monotonic()>=deadline:
                    emit(124,timed_out=True)
                block=source.read(65536)
                if not block: break
                digest.update(block)
        if digest.hexdigest()!=expected:
            raise ValueError('traineddata digest changed')
    provenance={'image_sha256':args.png_sha256,'image_bytes':len(data),'width':width,'height':height,
        'manifest_sha256':hashlib.sha256(raw_manifest).hexdigest(),'manifest':manifest,'language':args.language}
    resource.setrlimit(resource.RLIMIT_CORE,(0,0))
    resource.setrlimit(resource.RLIMIT_FSIZE,(4*1024*1024,4*1024*1024))
    # /tmp is root-owned 0711. Child can traverse/read this immutable file but
    # cannot replace it or modify the monitor. Only /tmp/work is child-writable.
    descriptor=os.open('/tmp/input.png',os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o444)
    with os.fdopen(descriptor,'wb') as destination:
        destination.write(data)
    os.mkdir('/tmp/work',0o777)
    os.chmod('/tmp/work',0o1777)
    if time.monotonic()>=deadline:
        emit(124,timed_out=True,provenance=provenance)
    child=subprocess.Popen(['/usr/bin/tesseract','/tmp/input.png','stdout','-l',args.language,'--psm','6','--oem','1','--dpi','300'],
        stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,start_new_session=True,
        user=65534,group=65534,extra_groups=[],cwd='/tmp/work',
        env={'PATH':'/usr/bin:/bin','HOME':'/tmp/work','TMPDIR':'/tmp/work','LANG':'C.UTF-8','OMP_THREAD_LIMIT':'1','OMP_NUM_THREADS':'1','TESSDATA_PREFIX':'/usr/share/tesseract-ocr/5/tessdata'})
    buffers={'text':bytearray(),'stderr':bytearray()}
    with selectors.DefaultSelector() as selector:
        for name,stream in [('text',child.stdout),('stderr',child.stderr)]:
            os.set_blocking(stream.fileno(),False)
            selector.register(stream,selectors.EVENT_READ,name)
        while selector.get_map():
            remaining=deadline-time.monotonic()
            if remaining<=0:
                emit(124,error=bytes(buffers['stderr']),timed_out=True,provenance=provenance)
            for key,_ in selector.select(min(remaining,0.05)):
                block=os.read(key.fileobj.fileno(),4096)
                if not block:
                    selector.unregister(key.fileobj);key.fileobj.close();continue
                capacity=min(args.max_output_bytes-sum(map(len,buffers.values())),
                    (args.max_text_bytes if key.data=='text' else 4096)-len(buffers[key.data]))
                buffers[key.data].extend(block[:max(0,capacity)])
                if len(block)>capacity:
                    emit(125,error=bytes(buffers['stderr']),output_limited=True,provenance=provenance)
    try:
        code=child.wait(timeout=max(0.001,deadline-time.monotonic()))
    except subprocess.TimeoutExpired:
        emit(124,error=bytes(buffers['stderr']),timed_out=True,provenance=provenance)
    text=bytes(buffers['text'])
    text.decode('utf-8',errors='strict')
    provenance['text_sha256']=hashlib.sha256(text).hexdigest()
    emit(code,text if code==0 else b'',bytes(buffers['stderr']),provenance=provenance)


if __name__=='__main__':
    try:
        main()
    except Exception as error:
        emit(126,error=('OCR supervisor refused input/contract: '+type(error).__name__).encode('ascii'))
