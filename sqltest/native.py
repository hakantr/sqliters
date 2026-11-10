"""ctypes backend for an explicitly verified pinned library. No system sqlite import."""
import ctypes as C
import struct
import os
from pathlib import Path
from support import out,environment


class SqlError(RuntimeError):
    def __init__(self, code, extended, message, offset, stage):
        self.code,self.extended,self.message,self.offset,self.stage=code,extended,message,offset,stage
        super().__init__(f'{stage}: {code}/{extended} offset={offset}: {message}')


def token(value):
    if value is None:return 'N'
    if isinstance(value,int):return 'I:'+str(value)
    if isinstance(value,float):return 'R:'+struct.pack('>d',value).hex()
    if isinstance(value,str):return 'T:'+value.encode().hex()
    return 'B:'+bytes(value).hex()


def wire(row):return ('\t'.join(token(v) for v in row)+'\n').encode('ascii')


class Database:
    def __init__(self, library, path):
        # SQLite reads libc getenv in this Python process, not only child env.
        for key in ['SQLITE_TMPDIR','TMPDIR']:
            os.environ[key]=environment()[key];os.putenv(key,environment()[key])
        self.lib=C.CDLL(str(library));self.db=C.c_void_p();self.path=path
        signatures={
          'sqlite3_open_v2':([C.c_char_p,C.POINTER(C.c_void_p),C.c_int,C.c_char_p],C.c_int),
          'sqlite3_close':([C.c_void_p],C.c_int),
          'sqlite3_prepare_v2':([C.c_void_p,C.c_char_p,C.c_int,C.POINTER(C.c_void_p),C.POINTER(C.c_char_p)],C.c_int),
          'sqlite3_step':([C.c_void_p],C.c_int),'sqlite3_reset':([C.c_void_p],C.c_int),
          'sqlite3_finalize':([C.c_void_p],C.c_int),'sqlite3_clear_bindings':([C.c_void_p],C.c_int),
          'sqlite3_bind_parameter_count':([C.c_void_p],C.c_int),'sqlite3_column_count':([C.c_void_p],C.c_int),'sqlite3_column_type':([C.c_void_p,C.c_int],C.c_int),
          'sqlite3_column_int64':([C.c_void_p,C.c_int],C.c_int64),'sqlite3_column_double':([C.c_void_p,C.c_int],C.c_double),
          'sqlite3_column_blob':([C.c_void_p,C.c_int],C.c_void_p),'sqlite3_column_text':([C.c_void_p,C.c_int],C.c_void_p),
          'sqlite3_column_bytes':([C.c_void_p,C.c_int],C.c_int),
          'sqlite3_bind_null':([C.c_void_p,C.c_int],C.c_int),
          'sqlite3_bind_int64':([C.c_void_p,C.c_int,C.c_int64],C.c_int),
          'sqlite3_bind_double':([C.c_void_p,C.c_int,C.c_double],C.c_int),
          'sqlite3_bind_text':([C.c_void_p,C.c_int,C.c_void_p,C.c_int,C.c_void_p],C.c_int),
          'sqlite3_bind_blob':([C.c_void_p,C.c_int,C.c_void_p,C.c_int,C.c_void_p],C.c_int),
          'sqlite3_errmsg':([C.c_void_p],C.c_char_p),'sqlite3_extended_errcode':([C.c_void_p],C.c_int),
          'sqlite3_error_offset':([C.c_void_p],C.c_int),'sqlite3_limit':([C.c_void_p,C.c_int,C.c_int],C.c_int),
          'sqlite3_get_autocommit':([C.c_void_p],C.c_int),'sqlite3_changes':([C.c_void_p],C.c_int),
          'sqlite3_last_insert_rowid':([C.c_void_p],C.c_int64)}
        for name,(args,result) in signatures.items():
            f=getattr(self.lib,name);f.argtypes=args;f.restype=result
        if str(path)!=':memory:':out(Path(path))
        self.check(self.lib.sqlite3_open_v2(str(path).encode(),C.byref(self.db),6,None),'open')

    def check(self,rc,stage):
        if rc!=0:
            raise SqlError(rc&255,self.lib.sqlite3_extended_errcode(self.db),self.lib.sqlite3_errmsg(self.db).decode('utf-8','replace'),self.lib.sqlite3_error_offset(self.db),stage)

    def close(self):
        if self.db:self.check(self.lib.sqlite3_close(self.db),'close');self.db=None

    def prepare(self,sql):return Statement(self,sql)

    def execute(self,sql):
        # Real sqlite tail handling. Comment-only/empty statements are skipped.
        data=sql.encode();buffer=C.create_string_buffer(data);offset=0
        while offset<len(data):
            stmt=C.c_void_p();tail=C.c_char_p();ptr=C.cast(C.byref(buffer,offset),C.c_char_p)
            rc=self.lib.sqlite3_prepare_v2(self.db,ptr,len(data)-offset,C.byref(stmt),C.byref(tail))
            self.check(rc,'prepare')
            next_offset=(C.cast(tail,C.c_void_p).value or C.addressof(buffer)+len(data))-C.addressof(buffer)
            if next_offset<=offset:raise ValueError('SQLite tail made no progress')
            offset=next_offset
            if not stmt:continue
            statement=Statement(self,None,stmt)
            try:
                yield from statement.rows()
            except BaseException as error:
                error.cleanup_finalize_rc=statement.close(ignore_error=True)
                raise
            else:statement.close()

    def exec(self,sql):
        for _ in self.execute(sql):pass


class Statement:
    def __init__(self,db,sql,handle=None):
        self.db=db;self.lib=db.lib;self.handle=handle or C.c_void_p()
        if handle is None:
            data=sql.encode();tail=C.c_char_p()
            db.check(self.lib.sqlite3_prepare_v2(db.db,data,len(data),C.byref(self.handle),C.byref(tail)),'prepare')
            if not self.handle:raise ValueError('empty prepared template')
            if tail.value and tail.value.strip():self.close();raise ValueError('prepared template has unexpected SQL tail')

    def bind(self,values):
        if len(values)!=self.lib.sqlite3_bind_parameter_count(self.handle):raise ValueError('bind parameter count mismatch')
        self.db.check(self.lib.sqlite3_clear_bindings(self.handle),'clear_bindings')
        transient=C.c_void_p(-1)
        for i,v in enumerate(values,1):
            if v is None:rc=self.lib.sqlite3_bind_null(self.handle,i)
            elif isinstance(v,int):rc=self.lib.sqlite3_bind_int64(self.handle,i,v)
            elif isinstance(v,float):rc=self.lib.sqlite3_bind_double(self.handle,i,v)
            else:
                b=v.encode() if isinstance(v,str) else bytes(v);buf=C.create_string_buffer(b)
                f=self.lib.sqlite3_bind_text if isinstance(v,(str,TextBytes)) else self.lib.sqlite3_bind_blob
                rc=f(self.handle,i,C.cast(buf,C.c_void_p),len(b),transient)
            self.db.check(rc,'bind')

    def rows(self):
        while True:
            rc=self.lib.sqlite3_step(self.handle)
            if rc==101:return
            if rc!=100:self.db.check(rc,'step')
            values=[]
            for i in range(self.lib.sqlite3_column_count(self.handle)):
                tag=self.lib.sqlite3_column_type(self.handle,i)
                if tag==5:v=None
                elif tag==1:v=int(self.lib.sqlite3_column_int64(self.handle,i))
                elif tag==2:v=float(self.lib.sqlite3_column_double(self.handle,i))
                else:
                    ptr=self.lib.sqlite3_column_text(self.handle,i) if tag==3 else self.lib.sqlite3_column_blob(self.handle,i)
                    n=self.lib.sqlite3_column_bytes(self.handle,i)
                    b=C.string_at(ptr,n) if n else b''
                    # Text bytes must survive invalid UTF-8. This wrapper's wire
                    # path tags them explicitly rather than silently replacing.
                    v=TextBytes(b) if tag==3 else b
                values.append(v)
            yield values

    def run(self):
        for _ in self.rows():pass

    def reset(self):self.db.check(self.lib.sqlite3_reset(self.handle),'reset')
    def close(self,ignore_error=False):
        if self.handle:
            handle=self.handle;self.handle=None
            rc=self.lib.sqlite3_finalize(handle)
            if not ignore_error:self.db.check(rc,'finalize')
            return rc


class TextBytes(bytes):pass
_original_token=token

def token(value):
    if isinstance(value,TextBytes):return 'T:'+bytes(value).hex()
    return _original_token(value)
