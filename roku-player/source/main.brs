' Volume Sync Player: plays videos handed over by the Volume Sync app.
' Launch:  POST /launch/dev?n=<count>&u1=<url>&t1=<title>&f1=<mp4|mkv|hls|ts>&s1=<subtitle url>&u2=...
'          POST /launch/dev?cal=<picture url>&every=<seconds>[&save=1]   (calendar; save=1 also keeps it for the screensaver)
' Running: POST /input?n=...   /input?cal=...   /input?seek=<ms>   /input?control=<play|pause|resume|stop>
sub Main(args as Dynamic)
    screen = CreateObject("roSGScreen")
    port = CreateObject("roMessagePort")
    screen.setMessagePort(port)
    input = CreateObject("roInput")
    input.setMessagePort(port)
    scene = screen.CreateScene("PlayerScene")
    screen.show()
    saveCalendar(args)
    scene.launchArgs = args
    while true
        msg = wait(0, port)
        t = type(msg)
        if t = "roSGScreenEvent"
            if msg.isScreenClosed() then return
        else if t = "roInputEvent"
            info = msg.getInfo()
            saveCalendar(info)
            scene.inputArgs = info
        end if
    end while
end sub

' Remember the calendar's address for the screensaver (registry lives on this thread).
sub saveCalendar(args as Dynamic)
    if args = invalid or args.cal = invalid or args.save <> "1" then return
    sec = CreateObject("roRegistrySection", "calendar")
    sec.Write("url", args.cal)
    every = "60"
    if args.every <> invalid then every = args.every
    sec.Write("every", every)
    sec.Flush()
end sub

' The Roku screensaver: Settings > Theme > Screensaver > Calendar (Volume Sync).
sub RunScreenSaver()
    screen = CreateObject("roSGScreen")
    port = CreateObject("roMessagePort")
    screen.setMessagePort(port)
    scene = screen.CreateScene("CalendarSaverScene")
    screen.show()
    sec = CreateObject("roRegistrySection", "calendar")
    if sec.Exists("url") and sec.Read("url") <> ""
        scene.every = Int(Val(sec.Read("every")))
        scene.url = sec.Read("url")
    else
        scene.message = "Turn on the calendar screensaver in the Volume Sync app on your PC."
    end if
    while true
        msg = wait(0, port)
        if type(msg) = "roSGScreenEvent" and msg.isScreenClosed() then return
    end while
end sub
