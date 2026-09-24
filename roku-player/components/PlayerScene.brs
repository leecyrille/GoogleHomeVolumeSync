sub init()
    m.video = m.top.findNode("video")
    m.hint = m.top.findNode("hint")
    m.video.observeField("state", "onState")
end sub

' Builds a playlist from n / u1 t1 f1 s1 ... parameters and starts it.
sub playFrom(args as Object)
    if args = invalid or args.n = invalid then return
    count = Val(args.n)
    if count < 1 then return
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
