// Only synthetic test windows; exits automatically. It never inspects other applications.
import AppKit
import CoreGraphics
import AVFoundation
let mediaMode = ProcessInfo.processInfo.environment["RP_MEDIA_FIXTURE"] == "1"
let mediaNonce = ProcessInfo.processInfo.environment["RP_WINDOW_FIXTURE_NONCE"] ?? UUID().uuidString
var mediaEngine: AVAudioEngine?
var toneRunning = false
var toneError = ""
final class TonePhase { var value: Double = 0 }
func stopFixture() { mediaEngine?.stop(); app.terminate(nil) }
let root = URL(fileURLWithPath: ProcessInfo.processInfo.environment["RP_WINDOW_FIXTURE_DIR"]!)
var windows: [String:NSWindow] = [:]
var clicks = ["A":0,"B":0]
var keys = ["A":0,"B":0]
var mouseUps = ["A":0,"B":0]
var keyUps = ["A":0,"B":0]
var heldKeys = ["A":Set<Int>(),"B":Set<Int>()]
var heldButtons = ["A":Set<Int>(),"B":Set<Int>()]
var keyUpSourcePID = ["A":Int64(0),"B":Int64(0)]
var mouseUpSourcePID = ["A":Int64(0),"B":Int64(0)]
var lastKey = ["A":"","B":""]
var clickSourcePID = ["A":Int64(0),"B":Int64(0)]
var keySourcePID = ["A":Int64(0),"B":Int64(0)]
var points: [String:[Double]] = [:]
var inputEvents: [[String:Any]] = []
var text = ["A":"","B":""]
var moves = ["A":0,"B":0]
var drags = ["A":0,"B":0]
var scrolls = ["A":0,"B":0]
func record(_ kind:String,_ label:String,_ event:NSEvent) {
    inputEvents.append(["kind":kind,"window":label,"source_pid":event.cgEvent?.getIntegerValueField(.eventSourceUnixProcessID) ?? 0,
        "monotonic_ns":DispatchTime.now().uptimeNanoseconds,"key_code":([NSEvent.EventType.keyDown,.keyUp,.flagsChanged].contains(event.type) ? event.keyCode : 0),
        "characters":([NSEvent.EventType.keyDown,.keyUp].contains(event.type) ? event.characters ?? "" : ""),"flags":event.modifierFlags.rawValue,
        "x":event.locationInWindow.x,"y":event.locationInWindow.y,
        "scroll_x":(event.type == .scrollWheel ? event.scrollingDeltaX : 0),"scroll_y":(event.type == .scrollWheel ? event.scrollingDeltaY : 0)])
    if inputEvents.count>256 {inputEvents.removeFirst(inputEvents.count-256)}
}
var closed = Set<String>()
var lastCommand = ""
var tick = 0
func save() {
    var data: [String:Any] = ["pid":ProcessInfo.processInfo.processIdentifier,"tick":tick,"command":lastCommand]
    if mediaMode { data["media_fixture"] = true; data["tone_running"] = toneRunning; data["tone_error"] = toneError }
    data["fixture_active"] = NSRunningApplication.current.isActive
    data["input_events"] = inputEvents
    data["text"] = text
    data["moves"] = moves
    data["drags"] = drags
    data["scrolls"] = scrolls
    let main = CGDisplayBounds(CGMainDisplayID())
    for (label,window) in windows {
        let frame=window.frame
        data[label] = ["id":window.windowNumber,"title":window.title,"clicks":clicks[label]!,"keys":keys[label]!,"mouse_ups":mouseUps[label]!,"key_ups":keyUps[label]!,"held_keys":heldKeys[label]!.sorted(),"held_buttons":heldButtons[label]!.sorted(),"key_up_source_pid":keyUpSourcePID[label]!,"mouse_up_source_pid":mouseUpSourcePID[label]!,"last_key":lastKey[label]!,"click_source_pid":clickSourcePID[label]!,"key_source_pid":keySourcePID[label]!,"last_point":points[label] ?? [],"closed":closed.contains(label),"key_window":window.isKeyWindow,"frame":[frame.minX,main.height-frame.maxY,frame.width,frame.height]] as [String:Any]
    }
    if let bytes=try? JSONSerialization.data(withJSONObject:data,options:[.sortedKeys]) {try? bytes.write(to:root.appendingPathComponent("state.json"),options:.atomic)}
}
class FixtureView:NSView {
    let label:String
    init(_ label:String){self.label=label;super.init(frame:.zero)}
    required init?(coder:NSCoder){fatalError("not used")}
    override var acceptsFirstResponder:Bool{true}
    override func acceptsFirstMouse(for event:NSEvent?)->Bool {true}
    override func mouseDown(with event:NSEvent) {
        record("mouse_down",label,event)
        heldButtons[label]!.insert(event.buttonNumber)
        clicks[label]! += 1;clickSourcePID[label]=event.cgEvent?.getIntegerValueField(.eventSourceUnixProcessID) ?? 0;window?.makeFirstResponder(self)
        if let window=window {
            let p=window.convertPoint(toScreen:event.locationInWindow)
            points[label] = [(p.x-window.frame.minX)/window.frame.width,(window.frame.maxY-p.y)/window.frame.height]
        }
        save();needsDisplay=true
    }
    override func mouseUp(with event:NSEvent){record("mouse_up",label,event);heldButtons[label]!.remove(event.buttonNumber);mouseUps[label]! += 1;mouseUpSourcePID[label]=event.cgEvent?.getIntegerValueField(.eventSourceUnixProcessID) ?? 0;save()}
    override func keyUp(with event:NSEvent){record("key_up",label,event);heldKeys[label]!.remove(Int(event.keyCode));keyUps[label]! += 1;keyUpSourcePID[label]=event.cgEvent?.getIntegerValueField(.eventSourceUnixProcessID) ?? 0;save()}
    override func keyDown(with event:NSEvent){record("key_down",label,event);text[label]! += event.characters ?? "";if text[label]!.count>512 {text[label] = String(text[label]!.suffix(512))};heldKeys[label]!.insert(Int(event.keyCode));keys[label]! += 1;keySourcePID[label]=event.cgEvent?.getIntegerValueField(.eventSourceUnixProcessID) ?? 0;lastKey[label]=event.characters ?? "";save();needsDisplay=true}
    override func mouseMoved(with event:NSEvent){moves[label]! += 1;record("mouse_move",label,event);save()}
    override func mouseDragged(with event:NSEvent){drags[label]! += 1;record("mouse_drag",label,event);save()}
    override func scrollWheel(with event:NSEvent){scrolls[label]! += 1;record("scroll",label,event);save()}
    override func flagsChanged(with event:NSEvent){record("flags",label,event);save()}
    override func draw(_ rect:NSRect){
        (label == "A" ? NSColor(srgbRed:0.05,green:0.15,blue:0.8,alpha:1) : NSColor(srgbRed:0.05,green:0.7,blue:0.15,alpha:1)).setFill();bounds.fill()
        ("Synthetic window \(label)\nClicks \(clicks[label]!)   Keys \(keys[label]!)\nFrame \(tick)" as NSString).draw(at:NSPoint(x:25,y:35),withAttributes:[.font:NSFont.systemFont(ofSize:24),.foregroundColor:NSColor.white])
        NSColor.white.setFill();NSRect(x:CGFloat(20+tick%60),y:bounds.height-35,width:15,height:15).fill()
    }
}
let app=NSApplication.shared;app.setActivationPolicy(.regular)
for (label,x) in [("A",150.0),("B",800.0)] {
    let w=NSWindow(contentRect:NSRect(x:x,y:220,width:540,height:380),styleMask:[.titled,.closable,.resizable],backing:.buffered,defer:false)
    w.title = mediaMode ? "Weavelo Media Fixture \(mediaNonce) \(label)" : "Weavelo scoped-window acceptance " + (ProcessInfo.processInfo.environment["RP_WINDOW_FIXTURE_NONCE"] ?? "missing-nonce")
    w.acceptsMouseMovedEvents=true;w.isReleasedWhenClosed=false;w.contentView=FixtureView(label);w.makeKeyAndOrderFront(nil);windows[label]=w
}
if mediaMode {
    // Output only: no input node or microphone is opened. The selected window
    // and this quiet synthetic tone belong to the same fixture process.
    let engine = AVAudioEngine()
    let format = AVAudioFormat(standardFormatWithSampleRate: 48000, channels: 2)!
    let phase = TonePhase()
    let node = AVAudioSourceNode(format: format) { _, _, count, list in
        let buffers = UnsafeMutableAudioBufferListPointer(list)
        for frame in 0..<Int(count) {
            let sample = Float(sin(phase.value) * 0.02)
            phase.value = (phase.value + 2 * Double.pi * 440 / 48000).truncatingRemainder(dividingBy: 2 * Double.pi)
            for buffer in buffers {
                guard let data = buffer.mData?.assumingMemoryBound(to: Float.self) else { continue }
                for channel in 0..<Int(buffer.mNumberChannels) { data[frame * Int(buffer.mNumberChannels) + channel] = sample }
            }
        }
        return noErr
    }
    engine.attach(node); engine.connect(node, to: engine.mainMixerNode, format: format)
    engine.mainMixerNode.outputVolume = 0.2
    do { try engine.start(); mediaEngine = engine; toneRunning = true }
    catch { toneError = String(describing: error) }
}
app.activate(ignoringOtherApps:true)
Timer.scheduledTimer(withTimeInterval:0.1,repeats:true){_ in
    tick += 1
    if let data=try? Data(contentsOf:root.appendingPathComponent("command.json")),let cmd=try? JSONSerialization.jsonObject(with:data) as? [String:String],let id=cmd["id"],id != lastCommand {
        if cmd["action"]=="activate_A" {
            app.activate(ignoringOtherApps:true)
            windows["A"]?.makeKeyAndOrderFront(nil)
        }
        if cmd["action"]=="activate_B" {
            app.activate(ignoringOtherApps:true)
            windows["B"]?.makeKeyAndOrderFront(nil)
        }
        if cmd["action"]=="move_resize_A" {
            // The supported control mode is explicitly foreground-window control.
            // Restore A to the front for the geometry test, not as a hidden-input claim.
            windows["A"]?.setFrame(NSRect(x:300,y:150,width:680,height:500),display:true)
            windows["A"]?.makeKeyAndOrderFront(nil)
        }
        if cmd["action"]=="close_A" {windows["A"]?.orderOut(nil);closed.insert("A")}
        if cmd["action"]=="quit" {stopFixture()}
        lastCommand=id
    }
    for w in windows.values {w.contentView?.needsDisplay=true}
    save()
}
Timer.scheduledTimer(withTimeInterval:180,repeats:false){_ in stopFixture()}
save();app.run()
