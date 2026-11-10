"""Canonical typed wire parsing without loading any database library."""
import hashlib
import math
from pathlib import Path
import re
import struct
from support import out


def normalize_row(raw,fields):
    tokens=raw.decode('ascii').rstrip('\n').split('\t')
    if len(tokens)!=fields:raise ValueError('typed wire field count')
    result=[]
    for token in tokens:
        if token.startswith('RDEC:'):
            value=float(token[5:])
            if not math.isfinite(value):raise ValueError('CLI nonfinite REAL needs separate API contract')
            token='R:'+struct.pack('>d',value).hex()
        if token=='N':pass
        elif token.startswith('I:'):
            if not re.fullmatch(r'-?(0|[1-9][0-9]*)',token[2:]) or not -(1<<63)<=int(token[2:])<(1<<63):raise ValueError('typed int64')
        elif token.startswith('R:'):
            if not re.fullmatch('[0-9a-f]{16}',token[2:]):raise ValueError('typed REAL bits')
        elif token.startswith(('T:','B:')):
            if not re.fullmatch('(?:[0-9a-f]{2})*',token[2:]):raise ValueError('typed byte hex')
        else:raise ValueError('typed wire tag')
        result.append(token)
    return ('\t'.join(result)+'\n').encode()


def normalize_file(source,destination,fields):
    count=0;h=hashlib.sha256()
    with Path(source).open('rb') as src,out(destination).open('wb') as dst:
        for raw in src:
            b=normalize_row(raw,fields);dst.write(b);h.update(b);count+=1
    return {'rows':count,'fields':count*fields,'sha256':h.hexdigest(),'path':str(destination)}


def inspect(path,fields):
    count=0;h=hashlib.sha256()
    with Path(path).open('rb') as stream:
        for row in stream:
            if normalize_row(row,fields)!=row:raise ValueError('noncanonical artifact')
            h.update(row);count+=1
    return {'rows':count,'fields':count*fields,'sha256':h.hexdigest()}


def query_payload(data):
    marker=b'SQLTEST_QUERY_BEGIN\n'
    if data.count(marker)!=1:raise ValueError('query configuration marker missing/duplicate')
    prefix,body=data.split(marker,1)
    if prefix.splitlines()[-3:]!=[b'wal',b'1',b'0']:raise ValueError('query per-connection configuration')
    return body
