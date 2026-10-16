import copy
import importlib.util
from pathlib import Path
import unittest
spec=importlib.util.spec_from_file_location("effects",Path(__file__).parents[1]/"acceptance/inspect_window_input.py")
effects=importlib.util.module_from_spec(spec);spec.loader.exec_module(effects)

class NativeInputEffects(unittest.TestCase):
    def fixture(self):
        window={"id":7,"title":"RemotePlay scoped-window acceptance "+"a"*32,"held_keys":[],"held_buttons":[]}
        before={"pid":9,"A":copy.deepcopy(window),"B":copy.deepcopy(window),"input_events":[],"text":{"B":""}}
        after=copy.deepcopy(before)
        events=[]
        def event(kind,**kw):
            e={"kind":kind,"window":"A","source_pid":42,"monotonic_ns":len(events),"x":0,"scroll_x":0,"scroll_y":0,"flags":0,"characters":"","key_code":0};e.update(kw);events.append(e)
        event("mouse_down");event("mouse_drag",x=30);event("mouse_drag",x=140);event("mouse_up");event("scroll",scroll_y=-3)
        for code,ch in zip([0,11,8,0],"abcA"):
            event("key_down",key_code=code,characters=ch,flags=(1<<17) if ch=="A" else 0);event("key_up",key_code=code)
        events.insert(-2,dict(events[-2],kind="flags",characters="",flags=1<<17))
        events.append(dict(events[-1],kind="flags",characters="",flags=0))
        for i,e in enumerate(events):e["monotonic_ns"]=i
        after["input_events"]=events
        sender={"requested_sequence":"drag_scroll_abc_shift_a","input_commands_enqueued":16,"cleanup_complete":True,"operation_succeeded":True,"fixture_pid":9,"fixture_window":7,"fixture_nonce":"a"*32}
        return before,after,sender
    def test_native_effects_required_and_reported(self):
        self.assertTrue(effects.verify(*self.fixture(),42)["verified"])
    def test_enqueue_without_native_events_rejected(self):
        b,a,s=self.fixture();a["input_events"]=[]
        with self.assertRaises(ValueError):effects.verify(b,a,s,42)
    def test_wrong_source_pid_rejected(self):
        with self.assertRaises(ValueError):effects.verify(*self.fixture(),43)
    def test_adjacent_window_delivery_rejected(self):
        b,a,s=self.fixture();a["input_events"][0]["window"]="B"
        with self.assertRaises(ValueError):effects.verify(b,a,s,42)
    def test_wrong_text_or_missing_key_up_rejected(self):
        for mutate in (lambda a:a["input_events"].pop(-2),lambda a:a["input_events"][5].update(characters="z")):
            b,a,s=self.fixture();mutate(a)
            with self.assertRaises(ValueError):effects.verify(b,a,s,42)
    def test_still_held_rejected(self):
        b,a,s=self.fixture();a["A"]["held_keys"]=[0]
        with self.assertRaises(ValueError):effects.verify(b,a,s,42)
    def test_fixture_replacement_and_ring_loss_rejected(self):
        for mutate in (lambda b,a:b.update(pid=10),lambda b,a:b.update(input_events=[{}])):
            b,a,s=self.fixture();mutate(b,a)
            with self.assertRaises(ValueError):effects.verify(b,a,s,42)

    def test_modifier_release_missing_rejected(self):
        b,a,s=self.fixture();a["input_events"].pop()
        with self.assertRaises(ValueError):effects.verify(b,a,s,42)
    def test_focus_requires_native_held_then_release_without_new_target_input(self):
        b,a,s=self.fixture();s["kind"]="controlled-synthetic-held-input-focus"
        b["A"].update(held_keys=[0],held_buttons=[0],key_window=True);a["B"]["key_window"]=True
        a["input_events"]=[dict(a["input_events"][0],kind="key_up",key_code=0),dict(a["input_events"][0],kind="mouse_up")]
        self.assertTrue(effects.verify_focus_release(b,a,s,42)["verified"])
        a["B"]["held_keys"]=[0]
        with self.assertRaises(ValueError):effects.verify_focus_release(b,a,s,42)
