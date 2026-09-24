sub init()
    m.posters = [m.top.findNode("a"), m.top.findNode("b")]
    m.front = -1        ' poster on screen, -1 before the first picture
    m.loading = -1      ' poster fetching the next picture
    m.lastOk = 0        ' when a picture last arrived (seconds)
    m.waiting = m.top.findNode("waiting")
    m.stale = m.top.findNode("stale")
    m.staleBg = m.top.findNode("staleBg")
    m.noteBg = m.top.findNode("noteBg")
    m.noteText = m.top.findNode("noteText")
    m.refresh = m.top.findNode("refresh")
    m.noteTimer = m.top.findNode("noteTimer")
    m.refresh.observeField("fire", "loadNext")
    m.noteTimer.observeField("fire", "hideNote")
    m.posters[0].observeField("loadStatus", "onLoaded0")
    m.posters[1].observeField("loadStatus", "onLoaded1")
end sub

function nowSeconds() as Integer
    return CreateObject("roDateTime").AsSeconds()
end function

sub onUrl()
    m.refresh.control = "stop"
    if m.top.url = "" then return
    ' Until the first picture arrives, try every few seconds.
    if m.front < 0 then m.refresh.duration = 5 else m.refresh.duration = interval()
    loadNext()
    m.refresh.control = "start"
end sub

function interval() as Integer
    every = m.top.every
    if every < 10 then every = 60
    return every
end function

sub loadNext()
    url = m.top.url
    if url = "" then return
    i = 0
    if m.front = 0 then i = 1
    m.loading = i
    sep = "?"
    if Instr(1, url, "?") > 0 then sep = "&"
    m.posters[i].uri = url + sep + "v=" + nowSeconds().toStr()
    ' Say so when the PC stops answering, since the clock in the picture stops too.
    if m.front >= 0 and nowSeconds() - m.lastOk > interval() * 3
        dt = CreateObject("roDateTime")
        dt.FromSeconds(m.lastOk)
        dt.ToLocalTime()
        m.stale.text = "Not updated since " + Right("0" + dt.GetHours().toStr(), 2) + ":" + Right("0" + dt.GetMinutes().toStr(), 2) + " · is the PC on?"
        m.stale.visible = true
        m.staleBg.visible = true
    end if
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
    if first
        m.refresh.control = "stop"
        m.refresh.duration = interval()
        m.refresh.control = "start"
    end if
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
