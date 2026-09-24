' Volume Sync Player: plays videos handed over by the Volume Sync app.
' Launch:  POST /launch/dev?n=<count>&u1=<url>&t1=<title>&f1=<mp4|mkv|hls|ts>&s1=<subtitle url>&u2=...
' Running: POST /input?n=...   /input?seek=<ms>   /input?control=<play|pause|resume|stop>
sub Main(args as Dynamic)
    screen = CreateObject("roSGScreen")
    port = CreateObject("roMessagePort")
    screen.setMessagePort(port)
    input = CreateObject("roInput")
    input.setMessagePort(port)
    scene = screen.CreateScene("PlayerScene")
    screen.show()
    scene.launchArgs = args
    while true
        msg = wait(0, port)
        t = type(msg)
        if t = "roSGScreenEvent"
            if msg.isScreenClosed() then return
        else if t = "roInputEvent"
            scene.inputArgs = msg.getInfo()
        end if
    end while
end sub
