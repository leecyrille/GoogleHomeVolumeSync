sub init()
    m.video = m.top.findNode("video")
    m.hint = m.top.findNode("hint")
    m.photo = m.top.findNode("photo")
    m.photoBg = m.top.findNode("photoBg")
    m.timer = m.top.findNode("slideTimer")
    m.slides = []
    m.slideIndex = 0
    m.audio = m.top.findNode("audio")
    m.nowPlaying = m.top.findNode("nowPlaying")
    m.cal = m.top.findNode("cal")
    m.video.observeField("state", "onState")
    m.audio.observeField("state", "onAudioState")
    m.audio.observeField("contentIndex", "onAudioIndex")
    m.timer.observeField("fire", "onSlideTimer")
end sub

' Builds a playlist from n / u1 t1 f1 s1 ... parameters. When the items are
' pictures (f = "image") it shows them as a slideshow instead.
sub playFrom(args as Object)
    if args = invalid or args.n = invalid then return
    count = Val(args.n)
    if count < 1 then return
    hideCalendar()
    if args.f1 = "image" then
        stopAudio()
        showPictures(args, count)
        return
    end if
    stopPictures()
    ' f = "audio:<format>" means music: play it without a video surface.
    if args.f1 <> invalid and Left(args.f1, 6) = "audio:" then
        playAudio(args, count)
        return
    end if
    stopAudio()
    playlist = CreateObject("roSGNode", "ContentNode")
    hasSubs = false
    for i = 1 to count
        url = args["u" + i.toStr()]
        if url <> invalid and url <> ""
            item = playlist.createChild("ContentNode")
            item.url = url
            title = args["t" + i.toStr()]
            if title <> invalid then item.title = title
            fmt = args["f" + i.toStr()]
            if fmt = invalid or fmt = "" then fmt = "mp4"
            item.streamFormat = fmt
            subUrl = args["s" + i.toStr()]
            if subUrl <> invalid and subUrl <> ""
                item.subtitleTracks = [{ Language: "eng", TrackName: subUrl, Description: "Subtitles" }]
                hasSubs = true
            end if
        end if
    end for
    if playlist.getChildCount() = 0 then return
    m.video.control = "stop"
    m.video.contentIsPlaylist = true
    m.video.content = playlist
    if hasSubs then m.video.globalCaptionMode = "On"
    m.video.visible = true
    m.hint.visible = false
    m.video.setFocus(true)
    ' ap=0 loads without starting, so several devices can start together.
    if args.ap = "0" then m.video.control = "prebuffer" else m.video.control = "play"
end sub

sub playAudio(args as Object, count as Integer)
    m.video.control = "stop"
    m.video.visible = false
    playlist = CreateObject("roSGNode", "ContentNode")
    for i = 1 to count
        url = args["u" + i.toStr()]
        f = args["f" + i.toStr()]
        if url <> invalid and url <> "" and f <> invalid
            item = playlist.createChild("ContentNode")
            item.url = url
            item.streamFormat = Mid(f, 7)
            title = args["t" + i.toStr()]
            if title <> invalid then item.title = title
        end if
    end for
    if playlist.getChildCount() = 0 then return
    m.audio.control = "stop"
    m.audio.contentIsPlaylist = true
    m.audio.content = playlist
    m.hint.visible = false
    m.nowPlaying.visible = true
    showAudioTitle()
    m.top.setFocus(true)
    if args.ap = "0" then m.audio.control = "prebuffer" else m.audio.control = "play"
end sub

sub stopAudio()
    m.audio.control = "stop"
    m.nowPlaying.visible = false
end sub

sub showAudioTitle()
    c = m.audio.content
    if c = invalid then return
    i = m.audio.contentIndex
    if i < 0 then i = 0
    item = c.getChild(i)
    if item <> invalid then m.nowPlaying.text = "♪" + Chr(10) + item.title
end sub

sub onAudioIndex()
    showAudioTitle()
end sub

sub onAudioState()
    if m.audio.state = "finished" then
        m.nowPlaying.visible = false
        m.hint.visible = true
    end if
end sub

' Pause / play / resume whichever player is in use.
sub control(action as String)
    if m.nowPlaying.visible
        target = m.audio
    else
        target = m.video
    end if
    if action = "play" and (target.state = "paused" or target.state = "none" or target.state = "stopped")
        if target.state = "paused" then action = "resume"
    end if
    target.control = action
end sub

sub showPictures(args as Object, count as Integer)
    m.video.control = "stop"
    m.video.visible = false
    m.hint.visible = false
    m.slides = []
    for i = 1 to count
        url = args["u" + i.toStr()]
        if url <> invalid and url <> "" then m.slides.push(url)
    end for
    if m.slides.count() = 0 then return
    m.slideIndex = 0
    m.photoBg.visible = true
    m.photo.visible = true
    m.photo.uri = m.slides[0]
    m.timer.control = "stop"
    if m.slides.count() > 1
        if args.iv <> invalid then m.timer.duration = Val(args.iv)
        m.timer.control = "start"
    end if
    m.top.setFocus(true)
end sub

sub stopPictures()
    m.timer.control = "stop"
    m.photo.visible = false
    m.photoBg.visible = false
    m.slides = []
end sub

sub showSlide(offset as Integer)
    n = m.slides.count()
    if n = 0 then return
    m.slideIndex = (m.slideIndex + offset + n) mod n
    m.photo.uri = m.slides[m.slideIndex]
end sub

sub onSlideTimer()
    showSlide(1)
end sub

' Left / right step through pictures (and restart the slideshow timer).
function onKeyEvent(key as String, press as Boolean) as Boolean
    if not press or m.slides.count() = 0 then return false
    if key = "right" or key = "fastforward"
        showSlide(1)
    else if key = "left" or key = "rewind"
        showSlide(-1)
    else
        return false
    end if
    if m.slides.count() > 1
        m.timer.control = "stop"
        m.timer.control = "start"
    end if
    return true
end function

' The calendar picture from the PC, refreshed every minute.
sub showCalendar(args as Object)
    stopAudio()
    stopPictures()
    m.video.control = "stop"
    m.video.visible = false
    m.hint.visible = false
    if args.every <> invalid then m.cal.every = Int(Val(args.every))
    m.cal.visible = true
    m.cal.url = args.cal
    if args.save = "1" then m.cal.note = "Saved as a screensaver. To use it: Home, then Settings › Theme › Screensaver › Calendar (Volume Sync)."
    m.top.setFocus(true)
end sub

sub hideCalendar()
    m.cal.url = ""
    m.cal.visible = false
end sub

sub onLaunch()
    a = m.top.launchArgs
    if a <> invalid and a.cal <> invalid then showCalendar(a) else playFrom(a)
end sub

sub onInput()
    a = m.top.inputArgs
    if a = invalid then return
    if a.cal <> invalid then
        showCalendar(a)
    else if a.n <> invalid then
        playFrom(a)
    else if a.seek <> invalid then
        if m.nowPlaying.visible then m.audio.seek = Val(a.seek) / 1000.0 else m.video.seek = Val(a.seek) / 1000.0
    else if a.control <> invalid then
        control(a.control)
    end if
end sub

sub onState()
    s = m.video.state
    if s = "finished" or s = "error" then
        m.video.visible = false
        m.hint.visible = true
        if s = "error" then
            m.hint.text = "Couldn't play this video." + Chr(10) + m.video.errorMsg
        else
            m.hint.text = "Volume Sync Player" + Chr(10) + "Choose Play videos in the Volume Sync app on your PC."
        end if
    end if
end sub
