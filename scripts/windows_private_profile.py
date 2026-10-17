"""Windows first-install private-directory creation and READ-ONLY repair plan.

The CLI has no real-profile apply/repair command. Mutating ACL regression is
restricted to an owned marked fixture beneath the user's temporary directory.
No authentication file contents are read by snapshot/plan_profile.
"""
import argparse
import ctypes
from ctypes import wintypes as W
import hashlib
import json
import os
from pathlib import Path
import stat
import sys
import tempfile

SYSTEM='S-1-5-18'
ADMINISTRATORS='S-1-5-32-544'
EXTRA='S-1-15-3-600613-1-2-4183550104-2345122513-2323214708'
FULL=0x1f01ff
INHERITED=0x10


def require(ok, message):
    if not ok:raise RuntimeError(message)


def derive_private_rows(rows, owner):
    """Preserve known rights/deny ACEs; remove only inherited foreign Allow ACEs."""
    kept=[];removed=[]
    allowed={owner,SYSTEM,ADMINISTRATORS}
    for row in rows:
        require(row['type'] in (0,1), 'Unsupported ACE; no automatic change')
        require(row['flags'] & ~0x1f == 0, 'Unsupported ACE flags')
        if row['type']==0 and row['sid'] not in allowed:
            require(row['flags'] & INHERITED, 'Explicit foreign grant requires separate review')
            removed.append(dict(row))
        else:
            kept.append(dict(row,flags=row['flags'] & ~INHERITED))
    for sid in allowed:
        require(any(x['type']==0 and x['sid']==sid and x['mask']==FULL for x in kept),
                'Required existing owner/system full-control grant missing; never invent rights')
    return kept,removed


def rows_dacl(rows):
    flags=[(1,'OI'),(2,'CI'),(4,'NP'),(8,'IO'),(16,'ID')]
    return 'D:P'+''.join('('+('A' if r['type']==0 else 'D')+';'+
        ''.join(name for bit,name in flags if r['flags']&bit)+';'+hex(r['mask'])+';;;'+r['sid']+')' for r in rows)


def private_sddl(owner):
    return 'O:'+owner+'D:P'+''.join('(A;OICI;FA;;;'+sid+')' for sid in [owner,SYSTEM,ADMINISTRATORS])


def ordinary_path(path):
    require(path.is_absolute() and '..' not in path.parts, 'Noncanonical profile path')
    for p in [path,*path.parents]:
        try:m=p.lstat()
        except FileNotFoundError:continue
        require(not p.is_symlink() and not getattr(m,'st_file_attributes',0)&0x400,
                'Linked/reparse profile path')


class NativeSecurity:
    def __init__(self):
        require(sys.platform=='win32','Native Windows required')
        self.k=ctypes.WinDLL('kernel32',use_last_error=True)
        self.a=ctypes.WinDLL('advapi32',use_last_error=True)
        self.z=ctypes.WinDLL('authz',use_last_error=True)
        self.P=ctypes.c_void_p
        P=self.P
        def bind(lib,name,args,result):
            f=getattr(lib,name);f.argtypes=args;f.restype=result;return f
        self.bind=bind
        self.free=bind(self.k,'LocalFree',[P],P)
        self.close=bind(self.k,'CloseHandle',[W.HANDLE],W.BOOL)
        self.current=bind(self.k,'GetCurrentProcess',[],W.HANDLE)
        self.open_token=bind(self.a,'OpenProcessToken',[W.HANDLE,W.DWORD,ctypes.POINTER(W.HANDLE)],W.BOOL)
        self.token_info=bind(self.a,'GetTokenInformation',[W.HANDLE,ctypes.c_int,P,W.DWORD,ctypes.POINTER(W.DWORD)],W.BOOL)
        self.sid_text=bind(self.a,'ConvertSidToStringSidW',[P,ctypes.POINTER(W.LPWSTR)],W.BOOL)
        self.string_sid=bind(self.a,'ConvertStringSidToSidW',[W.LPCWSTR,ctypes.POINTER(P)],W.BOOL)
        self.get_sd=bind(self.a,'GetNamedSecurityInfoW',[W.LPWSTR,ctypes.c_int,W.DWORD,*([ctypes.POINTER(P)]*5)],W.DWORD)
        self.get_control=bind(self.a,'GetSecurityDescriptorControl',[P,ctypes.POINTER(W.WORD),ctypes.POINTER(W.DWORD)],W.BOOL)
        self.sd_string=bind(self.a,'ConvertSecurityDescriptorToStringSecurityDescriptorW',[P,W.DWORD,W.DWORD,ctypes.POINTER(W.LPWSTR),ctypes.POINTER(W.DWORD)],W.BOOL)
        self.string_sd=bind(self.a,'ConvertStringSecurityDescriptorToSecurityDescriptorW',[W.LPCWSTR,W.DWORD,ctypes.POINTER(P),ctypes.POINTER(W.DWORD)],W.BOOL)
        self.get_ace=bind(self.a,'GetAce',[P,W.DWORD,ctypes.POINTER(P)],W.BOOL)
        self.make_directory=bind(self.k,'CreateDirectoryW',[W.LPCWSTR,P],W.BOOL)
        self.set_sd=bind(self.a,'SetNamedSecurityInfoW',[W.LPWSTR,ctypes.c_int,W.DWORD,P,P,P,P],W.DWORD)
        self.get_dacl=bind(self.a,'GetSecurityDescriptorDacl',[P,ctypes.POINTER(W.BOOL),ctypes.POINTER(P),ctypes.POINTER(W.BOOL)],W.BOOL)
        self.user=self.current_user()

    def sid(self, pointer):
        if not pointer:return None
        text=W.LPWSTR();require(self.sid_text(pointer,ctypes.byref(text)),'SID metadata unavailable')
        try:return text.value
        finally:self.free(ctypes.cast(text,self.P))

    def current_user(self):
        token=W.HANDLE();require(self.open_token(self.current(),8,ctypes.byref(token)),'Token user unavailable')
        try:
            n=W.DWORD();self.token_info(token,1,None,0,ctypes.byref(n));require(0<n.value<=65536,'Token metadata bounds')
            buffer=ctypes.create_string_buffer(n.value)
            require(self.token_info(token,1,buffer,n,ctypes.byref(n)),'Token metadata read failed')
            return self.sid(self.P.from_buffer(buffer).value)
        finally:self.close(token)

    def snapshot(self,path):
        ordinary_path(path);m=path.lstat()
        require(stat.S_ISDIR(m.st_mode) or (stat.S_ISREG(m.st_mode) and m.st_nlink==1),'Special/linked profile object')
        P=self.P;owner=P();group=P();dacl=P();sd=P()
        error=self.get_sd(str(path),1,7,ctypes.byref(owner),ctypes.byref(group),ctypes.byref(dacl),None,ctypes.byref(sd))
        require(error==0 and dacl.value,'Owner/group/DACL query failed')
        try:
            own=self.sid(owner);grp=self.sid(group);text=W.LPWSTR();n=W.DWORD()
            require(self.sd_string(sd,1,7,ctypes.byref(text),ctypes.byref(n)),'SDDL conversion failed')
            try:sddl=text.value
            finally:self.free(ctypes.cast(text,P))
            count=ctypes.c_uint16.from_address(dacl.value+4).value;require(count<=64,'Too many ACEs')
            rows=[]
            for index in range(count):
                ace=P();require(self.get_ace(dacl,index,ctypes.byref(ace)),'ACE query failed')
                kind=ctypes.c_ubyte.from_address(ace.value).value
                flags=ctypes.c_ubyte.from_address(ace.value+1).value
                size=ctypes.c_uint16.from_address(ace.value+2).value
                require(kind in (0,1) and size>=16,'Unsupported ACE type')
                rows.append({'type':kind,'flags':flags,'mask':ctypes.c_uint32.from_address(ace.value+4).value,'sid':self.sid(ace.value+8)})
            control=W.WORD();revision=W.DWORD();require(self.get_control(sd,ctypes.byref(control),ctypes.byref(revision)),'SD control unavailable')
            return {'path':str(path),'owner':own,'group':grp,'sddl':sddl,'dacl_protected':bool(control.value&0x1000),'aces':rows,
                'file_identity':{'device':m.st_dev,'inode':m.st_ino,'links':m.st_nlink,'size':m.st_size,'mtime_ns':m.st_mtime_ns},'contents_read':False}
        finally:self.free(sd)

    def create_private_directory(self,path):
        """Create only a nonexistent leaf, with protected security from first visibility."""
        ordinary_path(path);require(path.parent.is_dir(),'Existing parent required')
        require(self.snapshot(path.parent)['owner']==self.user,'Parent is not owned by this user')
        require(not path.exists() and not path.is_symlink(),'Existing profile is never repaired implicitly')
        P=self.P;sd=P();require(self.string_sd(private_sddl(self.user),1,ctypes.byref(sd),None),'Private SDDL invalid')
        class Attributes(ctypes.Structure):_fields_=[('length',W.DWORD),('security',P),('inherit',W.BOOL)]
        attributes=Attributes(ctypes.sizeof(Attributes),sd,False)
        try:
            if not self.make_directory(str(path),ctypes.byref(attributes)):
                raise OSError(ctypes.get_last_error(),'Private directory creation failed')
        finally:self.free(sd)
        result=self.snapshot(path)
        require(result['owner']==self.user and result['dacl_protected'],'New directory ownership/security drift')
        require(all(a['type']==0 and a['sid'] in {self.user,SYSTEM,ADMINISTRATORS} and a['mask']==FULL for a in result['aces']), 'New directory grants drift')
        return result

    def fixture_dacl(self,path,sddl,fixture_root):
        """This setter cannot address the real profile; only a marked Temp fixture."""
        ordinary_path(fixture_root);ordinary_path(path)
        require(fixture_root.parent==Path(tempfile.gettempdir()).resolve() and fixture_root.name.startswith('weavelo-acl-fixture-'),'Not an isolated Temp fixture')
        require((fixture_root/'.fixture-owner.json').is_file() and path.is_relative_to(fixture_root),'Fixture marker/scope absent')
        marker=json.loads((fixture_root/'.fixture-owner.json').read_text())
        require(marker=={'owner_sid':self.user,'purpose':'weavelo-isolated-acl-regression'},'Unknown fixture owner')
        require(self.snapshot(path)['owner']==self.user,'Fixture target owner changed')
        P=self.P;sd=P();require(self.string_sd(sddl,1,ctypes.byref(sd),None),'Fixture SDDL invalid')
        try:
            present=W.BOOL();default=W.BOOL();dacl=P()
            require(self.get_dacl(sd,ctypes.byref(present),ctypes.byref(dacl),ctypes.byref(default)) and present.value and dacl.value,'No fixture DACL')
            require(self.set_sd(str(path),1,0x80000004,None,None,dacl,None)==0,'Fixture DACL update failed')
        finally:self.free(sd)

    def grants(self,sddl,principal):
        P=self.P;DP=ctypes.POINTER(W.DWORD)
        class Luid(ctypes.Structure):_fields_=[('low',W.DWORD),('high',W.LONG)]
        class Request(ctypes.Structure):_fields_=[('desired',W.DWORD),('self_sid',P),('objects',P),('count',W.DWORD),('optional',P)]
        class Reply(ctypes.Structure):_fields_=[('count',W.DWORD),('granted',DP),('sacl',DP),('error',DP)]
        init=self.bind(self.z,'AuthzInitializeResourceManager',[W.DWORD,P,P,P,W.LPCWSTR,ctypes.POINTER(P)],W.BOOL)
        context=self.bind(self.z,'AuthzInitializeContextFromSid',[W.DWORD,P,P,P,Luid,P,ctypes.POINTER(P)],W.BOOL)
        check=self.bind(self.z,'AuthzAccessCheck',[W.DWORD,P,ctypes.POINTER(Request),P,P,P,W.DWORD,ctypes.POINTER(Reply),P],W.BOOL)
        free_context=self.bind(self.z,'AuthzFreeContext',[P],W.BOOL);free_manager=self.bind(self.z,'AuthzFreeResourceManager',[P],W.BOOL)
        rm=P();ctx=P();sid=P();sd=P()
        try:
            require(init(1,None,None,None,'WeaveloReadOnlyDaclCalculation',ctypes.byref(rm)),'Authz RM failure')
            require(self.string_sid(principal,ctypes.byref(sid)),'Authz SID failure')
            require(context(2,sid,rm,None,Luid(),None,ctypes.byref(ctx)),'Authz context failure')
            require(self.string_sd(sddl,1,ctypes.byref(sd),None),'Authz descriptor failure')
            granted=W.DWORD();sacl=W.DWORD();error=W.DWORD();req=Request(0x02000000,None,None,0,None);reply=Reply(1,ctypes.pointer(granted),ctypes.pointer(sacl),ctypes.pointer(error))
            require(check(0,ctx,ctypes.byref(req),None,sd,None,0,ctypes.byref(reply),None),'Authz check failed')
            return {'mask':hex(granted.value),'error':error.value,'read_data':bool(granted.value&1),'write_data':bool(granted.value&2)}
        finally:
            if sd:self.free(sd)
            if ctx:free_context(ctx)
            if sid:self.free(sid)
            if rm:free_manager(rm)


def plan_profile(native):
    root=Path.home()/'AppData/Roaming/Weavelo/Mesh'
    result=[]
    for path in [root,root/'mesh.conf',root/'mesh.secret']:
        before=native.snapshot(path);require(before['owner']==native.user,'Real profile owner differs')
        rows,removed=derive_private_rows(before['aces'],native.user)
        after='O:'+before['owner']+('G:'+before['group'] if before['group'] else '')+rows_dacl(rows)
        result.append({'path':str(path),'before':before,'after_sddl':after,'after_aces':rows,'after_dacl_protected':True,
            'removed_only_inherited_foreign_allow_aces':removed,'owner_or_group_change':False,
            'retained_ace_access_masks_unchanged':True,'contents_read_or_written':False,
            'extra_capability_before':native.grants(before['sddl'],EXTRA),'extra_capability_after':native.grants(after,EXTRA),
            'owner_before':native.grants(before['sddl'],native.user),'owner_after':native.grants(after,native.user)})
    return {'kind':'read-only-three-path-private-profile-acl-plan','current_user_sid':native.user,
        'paths':result,'real_acl_applied':False,'parent_roaming_change':False,
        'note':'No CLI exists to apply this real-profile plan; a separately authorized CAS-bound migration is required.'}


def isolated_regression(native):
    temp=Path(tempfile.gettempdir()).resolve();root=temp/('weavelo-acl-fixture-'+str(os.getpid()))
    native.create_private_directory(root)
    (root/'.fixture-owner.json').write_text(json.dumps({'owner_sid':native.user,'purpose':'weavelo-isolated-acl-regression'}))
    parent=root/'inherited-parent';native.create_private_directory(parent)
    broad=private_sddl(native.user)+'(A;OICI;0x1200a9;;;'+EXTRA+')'
    native.fixture_dacl(parent,broad,root)
    ordinary=parent/'ordinary-profile';ordinary.mkdir();(ordinary/'dummy-data').write_text('isolated dummy data')
    before=native.snapshot(ordinary/'dummy-data');require(native.grants(before['sddl'],EXTRA)['read_data'],'Fixture did not reproduce inherited extra read')
    secure=parent/'private-profile';created=native.create_private_directory(secure)
    (secure/'dummy-data').write_text('isolated dummy data')
    protected=native.snapshot(secure/'dummy-data')
    require(not native.grants(protected['sddl'],EXTRA)['read_data'],'Extra capability still reads new private file')
    for principal in [native.user,SYSTEM,ADMINISTRATORS]:require(native.grants(protected['sddl'],principal)['mask']==hex(FULL),'Required rights changed')
    require(not native.grants(protected['sddl'],'S-1-5-7')['read_data'],'Anonymous read broadened')
    old=native.snapshot(ordinary)
    try:native.create_private_directory(ordinary)
    except RuntimeError:pass
    else:raise RuntimeError('Existing broad profile was silently repaired')
    require(native.snapshot(ordinary)==old,'Existing fixture profile changed')
    return {'isolated_fixture':str(root),'passed':True,'insecure_inheritance_reproduced':True,
        'private_at_creation_verified':True,'new_private_directory_protected':created['dacl_protected'],
        'owner_system_administrators_full_control_preserved':True,'extra_and_anonymous_read_denied':True,
        'existing_broad_profile_refused_unchanged':True,'dummy_data_only':True,'real_profile_acl_changed':False}


def main():
    p=argparse.ArgumentParser();group=p.add_mutually_exclusive_group(required=True)
    group.add_argument('--plan-profile',action='store_true');group.add_argument('--isolated-regression',action='store_true')
    a=p.parse_args();native=NativeSecurity()
    if a.plan_profile:
        result=plan_profile(native)
    else:
        real=Path.home()/'AppData/Roaming/Weavelo/Mesh'
        targets=[p for p in [real,real/'mesh.conf',real/'mesh.secret'] if p.exists()]
        before=[native.snapshot(p) for p in targets]
        result=isolated_regression(native)
        require([native.snapshot(p) for p in targets]==before,'Real profile metadata changed during fixture')
        result['real_profile_metadata_and_acl_unchanged_verified']=True
        result['real_profile_sddl_sha256']=[{'path':b['path'],'sha256':hashlib.sha256(b['sddl'].encode()).hexdigest()} for b in before]
    print(json.dumps(result,indent=2))


if __name__=='__main__':main()
