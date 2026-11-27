import AVKit
import SwiftUI
import WebKit

/// How far a thumbnail or a file has got.
enum MediaLoad: Equatable {
    case loading(received: Int64, total: Int64)
    case ready(URL)
    case failed(String)

    var fraction: Double? {
        guard case .loading(let got, let total) = self, total > 0 else { return nil }
        return Double(got) / Double(total)
    }
}

// MARK: Fetching

extension Store {
    /// Where fetched media is kept: in Caches, so the system may clear it, by
    /// Mac and ref. A ref changes when the file does, so what's here is current.
    private func cached(_ media: Media, thumb: Bool) -> URL? {
        guard let mac else { return nil }
        let dir = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("media", isDirectory: true)
            .appendingPathComponent(String(mac.id.prefix(16)), isDirectory: true)
        let ext = media.ext.isEmpty ? "bin" : media.ext
        return dir.appendingPathComponent(thumb ? "\(media.ref).thumb.png" : "\(media.ref).\(ext)")
    }

    /// Fetch the picture standing for this media, unless it's here or on its way.
    func loadThumb(_ media: Media) {
        load(media, thumb: true)
    }

    /// Fetch the file itself, to show in full.
    func download(_ media: Media) {
        load(media, thumb: false)
    }

    /// Call off the file's download; what came is kept for next time.
    func cancelDownload(_ media: Media) {
        let key = "\(media.ref).full"
        if let id = fetching.removeValue(forKey: key) { bridge.cancelFetch(id) }
        if case .loading = downloads[media.ref] { downloads[media.ref] = nil }
    }

    private func load(_ media: Media, thumb: Bool) {
        let key = "\(media.ref).\(thumb ? "thumb" : "full")"
        let state = thumb ? thumbs[media.ref] : downloads[media.ref]
        if fetching[key] != nil { return }
        if case .ready = state { return }
        if thumb, case .failed = state { return }
        guard media.exists, let dest = cached(media, thumb: thumb) else { return }
        let set: (MediaLoad) -> Void = { [weak self] value in
            if thumb { self?.thumbs[media.ref] = value } else { self?.downloads[media.ref] = value }
        }
        if FileManager.default.fileExists(atPath: dest.path) {
            set(.ready(dest))
            return
        }
        set(.loading(received: 0, total: thumb ? 0 : media.size ?? 0))
        fetching[key] = bridge.fetch(token: media.ref, thumb: thumb, to: dest) { [weak self] event in
            switch event {
            case .progress(let got, let total):
                set(.loading(received: got, total: total))
            case .done:
                self?.fetching[key] = nil
                set(.ready(dest))
            case .failed(let reason):
                self?.fetching[key] = nil
                set(.failed(reason))
            }
        }
    }
}

// MARK: In the transcript

/// The pictures and videos a message points at, under it.
struct MediaGallery: View {
    let media: [Media]
    @State private var shown: Media?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(media) { item in
                if item.exists {
                    Button { shown = item } label: { MediaTile(media: item) }
                        .buttonStyle(.plain)
                } else {
                    MediaRow(media: item, note: "No longer on the Mac")
                }
            }
        }
        .fullScreenCover(item: $shown) { MediaViewer(media: $0) }
    }
}

/// A picture of the media, with a play mark and its size for a video.
private struct MediaTile: View {
    @Environment(Store.self) private var store
    let media: Media

    var body: some View {
        Group {
            switch store.thumbs[media.ref] {
            case .ready(let url):
                if let image = UIImage(contentsOfFile: url.path) {
                    Image(uiImage: image)
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                        .frame(maxWidth: .infinity, maxHeight: 260, alignment: .leading)
                        .clipShape(RoundedRectangle(cornerRadius: 10))
                        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.border))
                        .overlay(alignment: .bottomLeading) { if media.isVideo { badge } }
                        .overlay { if media.isVideo { play } }
                } else {
                    MediaRow(media: media, note: media.sizeText)
                }
            case .failed:
                // No picture for it (a WebM Quick Look can't draw, say): still tappable.
                MediaRow(media: media, note: media.sizeText)
            default:
                RoundedRectangle(cornerRadius: 10)
                    .fill(Theme.panel)
                    .frame(height: 140)
                    .overlay(ProgressView())
                    .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.border))
            }
        }
        .onAppear { store.loadThumb(media) }
    }

    private var play: some View {
        Image(systemName: "play.fill")
            .font(.system(size: 20))
            .foregroundStyle(.white)
            .frame(width: 52, height: 52)
            .background(.black.opacity(0.45), in: Circle())
    }

    private var badge: some View {
        Text([media.name, media.sizeText].compactMap { $0 }.joined(separator: " · "))
            .font(.caption2)
            .lineLimit(1)
            .foregroundStyle(.white)
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .background(.black.opacity(0.55), in: Capsule())
            .padding(8)
    }
}

/// A line standing for media there's no picture of.
private struct MediaRow: View {
    let media: Media
    let note: String?

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: media.isVideo ? "film" : "photo")
                .foregroundStyle(media.exists ? Theme.blue : Theme.quiet)
                .frame(width: 22)
            VStack(alignment: .leading, spacing: 1) {
                Text(media.name).font(.footnote).foregroundStyle(media.exists ? Theme.text : Theme.muted).lineLimit(1)
                if let note { Text(note).font(.caption2).foregroundStyle(Theme.quiet) }
            }
            Spacer(minLength: 0)
            if media.exists {
                Image(systemName: media.isVideo ? "play.circle" : "arrow.up.left.and.arrow.down.right")
                    .foregroundStyle(Theme.quiet)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .background(Theme.panel, in: RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.border))
    }
}

// MARK: Full screen

/// One picture or video, fetched in full and shown: pictures to zoom, videos
/// in the system player, or a web view for those it can't play (WebM).
struct MediaViewer: View {
    @Environment(Store.self) private var store
    @Environment(\.dismiss) private var dismiss
    let media: Media

    var body: some View {
        NavigationStack {
            content
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Color.black)
                .navigationTitle(media.name)
                .navigationBarTitleDisplayMode(.inline)
                .toolbarBackground(.visible, for: .navigationBar)
                .toolbar {
                    ToolbarItem(placement: .topBarLeading) {
                        Button("Done") { dismiss() }
                    }
                    if case .ready(let url) = store.downloads[media.ref] {
                        ToolbarItem(placement: .topBarTrailing) {
                            ShareLink(item: url)
                        }
                    }
                }
        }
        .onAppear { store.download(media) }
        .onDisappear {
            if case .loading = store.downloads[media.ref] { store.cancelDownload(media) }
        }
    }

    @ViewBuilder private var content: some View {
        switch store.downloads[media.ref] {
        case .ready(let url):
            if !media.isVideo, let image = UIImage(contentsOfFile: url.path) {
                ZoomableImage(image: image)
            } else if media.playsNatively {
                Player(url: url)
            } else {
                WebVideo(url: url)
            }
        case .failed(let reason):
            VStack(spacing: 12) {
                Image(systemName: "exclamationmark.triangle").font(.title2).foregroundStyle(Theme.amber)
                Text(reason).font(.footnote).foregroundStyle(.white.opacity(0.8)).multilineTextAlignment(.center)
                Button("Try again") {
                    store.downloads[media.ref] = nil
                    store.download(media)
                }
            }
            .padding(32)
        case .loading(let got, let total):
            VStack(spacing: 10) {
                if total > 0 {
                    ProgressView(value: Double(got), total: Double(total)).tint(.white).frame(width: 200)
                    Text("\(bytes(got)) of \(bytes(total))").font(.caption).foregroundStyle(.white.opacity(0.7)).monospacedDigit()
                } else {
                    ProgressView().tint(.white)
                }
            }
        case nil:
            ProgressView().tint(.white)
        }
    }

    private func bytes(_ n: Int64) -> String { ByteCountFormatter.string(fromByteCount: n, countStyle: .file) }
}

/// A video AVFoundation plays, started as it opens.
private struct Player: View {
    let url: URL
    @State private var player: AVPlayer?

    var body: some View {
        VideoPlayer(player: player)
            .onAppear {
                if player == nil { player = AVPlayer(url: url) }
                player?.play()
            }
            .onDisappear { player?.pause() }
    }
}

/// A picture to pinch and pan, double-tap to zoom.
private struct ZoomableImage: UIViewRepresentable {
    let image: UIImage

    func makeUIView(context: Context) -> UIScrollView {
        let scroll = UIScrollView()
        scroll.minimumZoomScale = 1
        scroll.maximumZoomScale = 6
        scroll.delegate = context.coordinator
        scroll.showsHorizontalScrollIndicator = false
        scroll.showsVerticalScrollIndicator = false
        let view = UIImageView(image: image)
        view.contentMode = .scaleAspectFit
        view.frame = scroll.bounds
        view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        scroll.addSubview(view)
        context.coordinator.view = view
        let tap = UITapGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.toggle(_:)))
        tap.numberOfTapsRequired = 2
        scroll.addGestureRecognizer(tap)
        return scroll
    }

    func updateUIView(_ scroll: UIScrollView, context: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator() }

    final class Coordinator: NSObject, UIScrollViewDelegate {
        weak var view: UIView?
        func viewForZooming(in scrollView: UIScrollView) -> UIView? { view }
        @objc func toggle(_ tap: UITapGestureRecognizer) {
            guard let scroll = tap.view as? UIScrollView else { return }
            if scroll.zoomScale > 1 {
                scroll.setZoomScale(1, animated: true)
            } else {
                let at = tap.location(in: view)
                scroll.zoom(to: CGRect(x: at.x - 60, y: at.y - 60, width: 120, height: 120), animated: true)
            }
        }
    }
}

/// A video AVFoundation can't play (WebM), played by WebKit instead, from a
/// page written beside it.
private struct WebVideo: UIViewRepresentable {
    let url: URL

    func makeUIView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()
        config.allowsInlineMediaPlayback = true
        config.mediaTypesRequiringUserActionForPlayback = []
        let web = WKWebView(frame: .zero, configuration: config)
        web.isOpaque = false
        web.backgroundColor = .black
        web.scrollView.isScrollEnabled = false
        let page = url.deletingPathExtension().appendingPathExtension("html")
        let name = url.lastPathComponent.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? url.lastPathComponent
        let html = """
        <!doctype html><meta name="viewport" content="width=device-width,initial-scale=1">
        <style>html,body{margin:0;height:100%;background:#000;display:flex;align-items:center;justify-content:center}
        video{max-width:100%;max-height:100%}p{color:#aaa;font:15px -apple-system;text-align:center;padding:24px}</style>
        <video src="\(name)" controls autoplay playsinline
          onerror="this.outerHTML='<p>This video can’t play on iPhone. Ask for it as an MP4.</p>'"></video>
        """
        try? html.write(to: page, atomically: true, encoding: .utf8)
        web.loadFileURL(page, allowingReadAccessTo: url.deletingLastPathComponent())
        return web
    }

    func updateUIView(_ web: WKWebView, context: Context) {}
}
