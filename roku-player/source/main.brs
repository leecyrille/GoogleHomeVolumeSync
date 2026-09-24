' Volume Sync Player: plays videos handed over by the Volume Sync app.
' Launch:  POST /launch/dev?n=<count>&u1=<url>&t1=<title>&f1=<mp4|mkv|hls|ts>&s1=<subtitle url>&u2=...
'          POST /launch/dev?cal=<base address>&theme=<dark|light>&views=<month,week,day>&rotate=<s>&q=<hd|4k>[&save=1]
'            (the calendar; save=1 also keeps these settings for the screensaver)
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

' The calendar settings the screensaver uses (registry lives on this thread).
function calendarKeys() as Object
    return ["cal", "theme", "views", "rotate", "q"]
end function

sub saveCalendar(args as Dynamic)
    if args = invalid or args.cal = invalid or args.save <> "1" then return
    sec = CreateObject("roRegistrySection", "calendar")
    for each k in calendarKeys()
        v = ""
        if args[k] <> invalid then v = args[k]
        sec.Write(k, v)
    end for
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
    if sec.Exists("cal") and sec.Read("cal") <> ""
        a = {}
        for each k in calendarKeys()
            if sec.Exists(k) then a[k] = sec.Read(k)
        end for
        scene.args = a
    else
        scene.message = "Turn on the calendar screensaver in the Volume Sync app on your PC."
    end if
    while true
        msg = wait(0, port)
        if type(msg) = "roSGScreenEvent" and msg.isScreenClosed() then return
    end while
end sub
