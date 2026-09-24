sub init()
    m.posters = [m.top.findNode("a"), m.top.findNode("b")]
    m.front = -1        ' poster on screen, -1 before the first picture
    m.loading = -1      ' poster fetching the next picture
    m.lastOk = 0        ' when a picture last arrived (seconds)
    m.base = ""
    m.allViews = ["month", "week", "day"]
    m.views = ["month"]
    m.view = "month"
    m.pausedUntil = 0   ' rotation waits after someone picks a view
    m.lastPing = 0
    m.vid = m.top.findNode("vid")
    m.bg = m.top.findNode("bg")
    m.waiting = m.top.findNode("waiting")
    m.stale = m.top.findNode("stale")
    m.staleBg = m.top.findNode("staleBg")
    m.noteBg = m.top.findNode("noteBg")
    m.noteText = m.top.findNode("noteText")
    m.refresh = m.top.findNode("refresh")
    m.rotateTimer = m.top.findNode("rotateTimer")
    m.noteTimer = m.top.findNode("noteTimer")
    m.refresh.observeField("fire", "onRefresh")
    m.rotateTimer.observeField("fire", "onRotate")
    m.noteTimer.observeField("fire", "hideNote")
    m.posters[0].observeField("loadStatus", "onLoaded0")
    m.posters[1].observeField("loadStatus", "onLoaded1")
    m.vid.observeField("state", "onVideoState")
end sub

function nowSeconds() as Integer
    return CreateObject("roDateTime").AsSeconds()
end function

function argOr(a as Object, k as String, fallback as String) as String
    if a <> invalid and a[k] <> invalid and a[k] <> "" then return a[k]
    return fallback
end function

sub onArgs()
    a = m.top.args
    m.refresh.control = "stop"
    m.rotateTimer.control = "stop"
    m.base = argOr(a, "cal", "")
    m.theme = argOr(a, "theme", "dark")
    m.fourK = argOr(a, "q", "hd") = "4k"
    m.rotate = Int(Val(argOr(a, "rotate", "0")))
    m.views = []
    for each v in argOr(a, "views", "month").Split(",")
        if v = "month" or v = "week" or v = "day" then m.views.push(v)
    end for
    if m.views.count() = 0 then m.views = ["month"]
    m.view = m.views[0]
    if m.base = "" then
        stopVideo()
        return
    end if
    if not m.fourK then stopVideo()
    loadNow()
    if m.rotate >= 10 and m.views.count() > 1
        m.rotateTimer.duration = m.rotate
        m.rotateTimer.control = "start"
    end if
end sub

function fileUrl(ext as String) as String
    return m.base + "calendar-" + m.theme + "-" + m.view + "." + ext + "?v=" + nowSeconds().toStr()
end function

' Fetch the current view now, then again just after each minute (when the PC makes new ones).
sub loadNow()
    if m.base = "" then return
    i = 0
    if m.front = 0 then i = 1
    m.loading = i
    m.posters[i].uri = fileUrl("jpg")
    showStaleIfNeeded()
    armRefresh()
end sub

sub armRefresh()
    m.refresh.control = "stop"
    if m.front < 0
        m.refresh.duration = 5          ' until the first picture arrives
    else
        m.refresh.duration = 64 - (nowSeconds() mod 60)
    end if
    m.refresh.control = "start"
end sub

sub onRefresh()
    loadNow()
end sub

sub onRotate()
    if nowSeconds() < m.pausedUntil or m.views.count() < 2 then return
    i = 0
    for j = 0 to m.views.count() - 1
        if m.views[j] = m.view then i = j
    end for
    m.view = m.views[(i + 1) mod m.views.count()]
    loadNow()
end sub

sub onLoaded0()
    onLoaded(0)
end sub

sub onLoaded1()
    onLoaded(1)
end sub

sub onLoaded(i as Integer)
    ' A failed fetch keeps the last picture on screen.
    if i <> m.loading or m.posters[i].loadStatus <> "ready" then return
    first = m.front < 0
    m.posters[i].opacity = 1.0
    if m.front >= 0 and m.front <> i then m.posters[m.front].opacity = 0.0
    m.front = i
    m.lastOk = nowSeconds()
    m.waiting.visible = false
    m.stale.visible = false
    m.staleBg.visible = false
    if first then armRefresh()
    ' 4K: the same picture as a video; the 1080p one covers the switch.
    if m.fourK
        m.bg.visible = true
        c = CreateObject("roSGNode", "ContentNode")
        c.url = fileUrl("mp4")
        c.streamFormat = "mp4"
        m.vid.content = c
        m.vid.visible = true
        m.vid.control = "play"
    end if
end sub

sub onVideoState()
    s = m.vid.state
    if s = "playing" and m.fourK
        ' Let the 4K video show through.
        m.bg.visible = false
        m.posters[0].opacity = 0.0
        m.posters[1].opacity = 0.0
    else if s = "error"
        stopVideo()
        if m.front >= 0 then m.posters[m.front].opacity = 1.0
    end if
end sub

sub stopVideo()
    m.vid.control = "stop"
    m.vid.visible = false
    m.bg.visible = true
end sub

sub showStaleIfNeeded()
    ' Say so when the PC stops answering, since the clock in the picture stops too.
    if m.front >= 0 and nowSeconds() - m.lastOk > 180
        dt = CreateObject("roDateTime")
        dt.FromSeconds(m.lastOk)
        dt.ToLocalTime()
        m.stale.text = "Not updated since " + Right("0" + dt.GetHours().toStr(), 2) + ":" + Right("0" + dt.GetMinutes().toStr(), 2) + " · is the PC on?"
        m.stale.visible = true
        m.staleBg.visible = true
    end if
end sub

' Up / Down: next or previous of month, week and day. Any button tells the PC someone is watching.
function onKeyEvent(key as String, press as Boolean) as Boolean
    if not press or m.base = "" then return false
    ping()
    if key = "up" or key = "down"
        i = 0
        for j = 0 to 2
            if m.allViews[j] = m.view then i = j
        end for
        if key = "down" then i = (i + 1) mod 3 else i = (i + 2) mod 3
        m.view = m.allViews[i]
        m.pausedUntil = nowSeconds() + 300
        loadNow()
        return true
    end if
    return false
end function

sub ping()
    if nowSeconds() - m.lastPing < 15 then return
    m.lastPing = nowSeconds()
    p = CreateObject("roSGNode", "Ping")
    p.url = m.base + "touch"
    p.control = "RUN"
end sub

sub onNote()
    if m.top.note = "" then return
    m.noteText.text = m.top.note
    m.noteText.visible = true
    m.noteBg.visible = true
    m.noteTimer.control = "stop"
    m.noteTimer.control = "start"
end sub

sub hideNote()
    m.noteText.visible = false
    m.noteBg.visible = false
end sub

sub onMessage()
    m.waiting.text = m.top.message
end sub
