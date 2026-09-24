sub init()
    m.video = m.top.findNode("video")
    m.hint = m.top.findNode("hint")
    m.photo = m.top.findNode("photo")
    m.photoBg = m.top.findNode("photoBg")
    m.timer = m.top.findNode("slideTimer")
    m.slides = []
    m.slideIndex = 0
    m.video.observeField("state", "onState")
    m.timer.observeField("fire", "onSlideTimer")
end sub

' Builds a playlist from n / u1 t1 f1 s1 ... parameters. When the items are
' pictures (f = "image") it shows them as a slideshow instead.
sub playFrom(args as Object)
    if args = invalid or args.n = invalid then return
    count = Val(args.n)
    if count < 1 then return
    if args.f1 = "image" then
        showPictures(args, count)
        return
    end if
    stopPictures()
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
    m.video.control = "play"
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

sub onLaunch()
    playFrom(m.top.launchArgs)
end sub

sub onInput()
    a = m.top.inputArgs
    if a = invalid then return
    if a.n <> invalid then
        playFrom(a)
    else if a.seek <> invalid then
        m.video.seek = Val(a.seek) / 1000.0
    else if a.control <> invalid then
        m.video.control = a.control
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
