import json, os, pathlib, plistlib, shutil, signal, socket, subprocess, tempfile, time, hashlib, sys

import argparse
parser=argparse.ArgumentParser(description='Disposable loopback process-lifecycle review; never changes launchd or the installed app.')
parser.add_argument('--binary', type=pathlib.Path, required=True)
parser.add_argument('--evidence-dir', type=pathlib.Path, required=True)
parser.add_argument('--timed-gui-exit', action='store_true')
args=parser.parse_args()
if not args.binary.is_file():parser.error('Build the background_lifecycle_review example first')
fixture = pathlib.Path(tempfile.mkdtemp(prefix='rp-bg-gui.', dir='/tmp'))
fixture.chmod(0o700)
profile = fixture / 'mesh'; profile.mkdir(mode=0o700)
review = args.evidence_dir.resolve(); review.mkdir(mode=0o700, parents=True, exist_ok=False)
timed_exit = args.timed_gui_exit
bundle = fixture / 'WeaveloLifecycle.app'; binary = bundle / 'Contents/MacOS/background_lifecycle_review'
binary.parent.mkdir(parents=True)
shutil.copy2(args.binary, binary)
(bundle / 'Contents/Info.plist').write_bytes(plistlib.dumps({'CFBundleIdentifier':'fun.hackerlife.weavelo.lifecycle-review',
    'CFBundleName':'Weavelo Lifecycle Review', 'CFBundleExecutable':binary.name,
    'CFBundlePackageType':'APPL', 'CFBundleVersion':'1', 'NSHighResolutionCapable':True}))
env = {k:v for k,v in os.environ.items() if not k.startswith('WEAVELO_')}
env.update(WEAVELO_DEVICE_GROUP_DIR=str(profile), WEAVELO_DISPLAY_NAME='Isolated lifecycle fixture',
           WEAVELO_HOST_BIND_ADDR='127.0.0.1:49163', WEAVELO_CLIENT_BIND_ADDR='127.0.0.1:0',
           WEAVELO_DISCOVERY_PORT='49157', WEAVELO_P2P='0', WEAVELO_RELAY='0',
           WEAVELO_DISCOVERY='0', WEAVELO_CLIENT_RECEIVER='0', WEAVELO_VIEWER_MEDIA='0',
           WEAVELO_CLIPBOARD_SYNC='0', WEAVELO_FILE_TRANSFER='0', WEAVELO_TALKBACK='0')
if timed_exit:
    env.update(WEAVELO_RESTORED_TEST_OUTPUT=str(review / 'gui-render-receipt.json'), WEAVELO_RESTORED_TEST_SECONDS='5')
test_socket = socket.socket(socket.AF_INET,socket.SOCK_DGRAM); test_socket.bind(('127.0.0.1',49163)); test_socket.close()
receipt = {'fixture':str(fixture),'bundle':str(bundle),'profile':str(profile),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),
           'system_launch_agent_modified':False,'other_devices_touched':False,'processes':[],'checks':{}}
receipt['gui_exit_trigger']='existing bounded product acceptance hook' if timed_exit else 'native window close'
receipt['manual_window_close_verified']=False
def save():
    (review / 'process-receipt.json').write_text(json.dumps(receipt,indent=2))
def run(*args, timeout=30):
    p=subprocess.run([str(binary),*args],cwd=fixture,env=env,capture_output=True,text=True,timeout=timeout)
    return p
def start(role):
    log=review / (role.strip('-') + '-' + str(len(receipt['processes'])) + '.log')
    fd=os.open(log,os.O_CREAT|os.O_EXCL|os.O_WRONLY,0o600)
    with os.fdopen(fd,'wb') as stream:
        p=subprocess.Popen([str(binary),role],cwd=fixture,env=env,stdout=stream,stderr=subprocess.STDOUT,start_new_session=True)
    receipt['processes'].append({'pid':p.pid,'role':role,'started_at':time.time()}); save(); return p
def status():
    p=run('--background-status-json'); return json.loads(p.stdout) if p.returncode==0 else None
def ready(p):
    deadline=time.monotonic()+40
    while time.monotonic()<deadline:
        if p.poll() is not None: raise RuntimeError('owned background fixture exited before ready')
        value=status()
        if value and value['pid']==p.pid:return value
        time.sleep(0.2)
    raise RuntimeError('fixture readiness timed out')
owned=[]
try:
    info=run('--product-info-json'); receipt['product_info']=json.loads(info.stdout)
    background=start('--background-service'); owned.append(background); ready(background)
    duplicate=run('--background-service'); receipt['checks']['second_background_rejected']=duplicate.returncode!=0
    probe=run('--check-loopback-host','127.0.0.1:49163'); receipt['checks']['background_connects_before_gui']=probe.returncode==0
    if probe.returncode==0:receipt['connection_probe']=json.loads(probe.stdout)
    gui=start('--gui'); owned.append(gui); receipt['gui_pid']=gui.pid; receipt['background_pid']=background.pid; receipt['state']='awaiting_native_gui_close'; save()
    print(json.dumps({'bundle':str(bundle),'gui_pid':gui.pid,'background_pid':background.pid,'checks':receipt['checks']}),flush=True)
    # The UI operator closes the actual native window. No fake UI or autoquit.
    try:gui.wait(timeout=180)
    except subprocess.TimeoutExpired:raise RuntimeError('native GUI close was not observed within the bounded review')
    receipt['checks']['gui_normal_exit']=gui.returncode==0
    if timed_exit:
        gui_receipt=json.loads((review/'gui-render-receipt.json').read_text())
        receipt['checks']['native_gui_rendered']=gui_receipt['render_calls']>0
        receipt['gui_render_calls']=gui_receipt['render_calls']
    else:receipt['manual_window_close_verified']=True
    receipt['checks']['background_survives_gui_exit']=background.poll() is None and status()['pid']==background.pid
    time.sleep(1)
    receipt['checks']['gui_not_restarted_by_fixture']=gui.poll()==0
    probe=run('--check-loopback-host','127.0.0.1:49163'); receipt['checks']['background_connects_after_gui_exit']=probe.returncode==0
    # Bounded crash recovery of this disposable background only; no launchd call.
    os.kill(background.pid, signal.SIGKILL); background.wait(timeout=10)
    offline_gui=run(timeout=10)
    receipt['checks']['default_gui_during_restart_gap_fails_without_host']=offline_gui.returncode!=0
    recovered=start('--background-service'); owned.append(recovered); ready(recovered)
    receipt['recovered_background_pid']=recovered.pid
    probe=run('--check-loopback-host','127.0.0.1:49163'); receipt['checks']['background_connects_after_fixture_crash_restart']=probe.returncode==0
    receipt['checks']['restart_command_has_no_gui']=receipt['processes'][-1]['role']=='--background-service'
    receipt['state']='complete'
except Exception as error:
    receipt['state']='failed'; receipt['error_type']=type(error).__name__; receipt['error_summary']=str(error)[:150]
finally:
    for p in reversed(owned):
        if p.poll() is None:
            p.send_signal(signal.SIGINT)
            try:p.wait(timeout=10)
            except subprocess.TimeoutExpired:p.terminate();p.wait(timeout=10)
        for item in receipt['processes']:
            if item['pid']==p.pid:item['exit_code']=p.returncode
    save(); print(json.dumps({'state':receipt['state'],'checks':receipt['checks'],'receipt':str(review/'process-receipt.json'),'error_summary':receipt.get('error_summary')}),flush=True)
