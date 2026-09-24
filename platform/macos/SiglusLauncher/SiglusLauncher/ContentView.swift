import SwiftUI
import AppKit

struct ContentView: View {
    @EnvironmentObject var library: GameLibrary
    @State private var dropTargeted = false

    private let columns: [GridItem] = [
        GridItem(.adaptive(minimum: 210, maximum: 280), spacing: 18),
    ]

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            ZStack {
                if library.games.isEmpty {
                    EmptyLibraryView()
                } else {
                    ScrollView(.vertical) {
                        LazyVGrid(columns: columns, spacing: 18) {
                            ForEach(library.visibleGames) { game in
                                GameTileView(game: game)
                            }
                        }
                        .padding(18)
                    }
                }
                if dropTargeted {
                    RoundedRectangle(cornerRadius: 14)
                        .strokeBorder(Color.accentColor, style: StrokeStyle(lineWidth: 3, dash: [10, 6]))
                        .background(Color.accentColor.opacity(0.08))
                        .padding(10)
                        .overlay(Text("Drop game folders to import").font(.title3).bold())
                }
                if library.isImporting {
                    Color.black.opacity(0.25)
                    VStack(spacing: 10) {
                        ProgressView()
                        Text("Scanning folders…")
                    }
                    .padding(20)
                    .background(RoundedRectangle(cornerRadius: 12).fill(Color(NSColor.windowBackgroundColor)))
                }
            }
            .onDrop(of: ["public.file-url"], isTargeted: $dropTargeted) { providers in
                loadDroppedFolders(providers)
                return true
            }
        }
        .frame(minWidth: 760, minHeight: 520)
        .alert(isPresented: $library.showError) {
            Alert(
                title: Text("Error"),
                message: Text(library.errorMessage),
                dismissButton: .default(Text("OK"))
            )
        }
        .sheet(item: $library.importReport) { report in
            ImportReportSheet(report: report)
        }
    }

    private var header: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Game Library")
                    .font(.title2)
                    .bold()
                Text("SiglusEngine · RealLive · AVG32 · UK2")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Spacer()

            TextField("Search", text: $library.searchText)
                .textFieldStyle(RoundedBorderTextFieldStyle())
                .frame(width: 200)

            Button {
                library.importGameFolders()
            } label: {
                Label("Import…", systemImage: "plus")
            }
            .keyboardShortcut("i", modifiers: [.command])
            .help("Import one or more game folders, or a folder that contains several games")

            Button {
                library.refreshAll()
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .keyboardShortcut("r", modifiers: [.command])
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 12)
    }

    private func loadDroppedFolders(_ providers: [NSItemProvider]) {
        let group = DispatchGroup()
        var urls: [URL] = []
        let lock = NSLock()
        for provider in providers {
            group.enter()
            provider.loadItem(forTypeIdentifier: "public.file-url", options: nil) { item, _ in
                defer { group.leave() }
                var url: URL? = nil
                if let data = item as? Data {
                    url = URL(dataRepresentation: data, relativeTo: nil)
                } else if let u = item as? URL {
                    url = u
                }
                if let url {
                    lock.lock()
                    urls.append(url)
                    lock.unlock()
                }
            }
        }
        group.notify(queue: .main) {
            Task { @MainActor in
                library.importFolders(urls)
            }
        }
    }
}

struct EmptyLibraryView: View {
    @EnvironmentObject var library: GameLibrary

    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: "square.stack.3d.up")
                .font(.system(size: 54))
                .foregroundColor(.secondary)
            Text("No games yet")
                .font(.title2)
                .bold()
            Text("Import a game folder, several folders at once, or a folder that contains many games.\nThe engine (SiglusEngine, RealLive, AVG32 or UK2) is detected automatically.")
                .multilineTextAlignment(.center)
                .foregroundColor(.secondary)
            Button("Import Games…") {
                library.importGameFolders()
            }
            .keyboardShortcut(.defaultAction)
            Text("You can also drop folders here.")
                .font(.caption)
                .foregroundColor(.secondary)
        }
        .padding(40)
    }
}

struct ImportReportSheet: View {
    @EnvironmentObject var library: GameLibrary
    let report: ImportReport

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(report.title)
                .font(.headline)
            ScrollView {
                Text(report.message)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .frame(maxHeight: 260)
            HStack {
                Spacer()
                Button("OK") {
                    library.importReport = nil
                }
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .frame(width: 460)
    }
}

struct EngineBadge: View {
    let engine: String
    let name: String

    private var color: Color {
        switch engine {
        case "siglus": return Color(red: 0.35, green: 0.45, blue: 0.95)
        case "reallive": return Color(red: 0.90, green: 0.45, blue: 0.25)
        case "avg32": return Color(red: 0.25, green: 0.65, blue: 0.45)
        case "uk2": return Color(red: 0.70, green: 0.35, blue: 0.80)
        default: return Color.gray
        }
    }

    var body: some View {
        Text(name)
            .font(.system(size: 10, weight: .semibold))
            .foregroundColor(.white)
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .background(Capsule().fill(color.opacity(0.92)))
    }
}

struct GameTileView: View {
    @EnvironmentObject var library: GameLibrary
    let game: GameEntry
    @State private var hovering = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ZStack(alignment: .topLeading) {
                cover
                    .frame(maxWidth: .infinity)
                    .frame(height: 150)
                    .clipped()
                    .clipShape(RoundedRectangle(cornerRadius: 12))

                EngineBadge(engine: game.engine, name: game.engineName)
                    .padding(8)

                if hovering {
                    RoundedRectangle(cornerRadius: 12)
                        .fill(Color.black.opacity(0.35))
                        .frame(height: 150)
                        .overlay(
                            Image(systemName: "play.circle.fill")
                                .font(.system(size: 46))
                                .foregroundColor(.white)
                        )
                }
            }
            .contentShape(Rectangle())
            .onTapGesture(count: 2) { library.launch(game: game) }
            .onHover { hovering = $0 }

            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(game.title)
                        .font(.headline)
                        .lineLimit(2)
                    if let label = game.nlsLabel {
                        Text(label)
                            .font(.caption2)
                            .foregroundColor(.secondary)
                            .lineLimit(1)
                    }
                }
                Spacer()
                Menu {
                    Button("Play") { library.launch(game: game) }
                    if !game.nlsOptions.isEmpty {
                        Menu("Text Encoding") {
                            ForEach(game.nlsOptions, id: \.id) { option in
                                Button((option.id == game.nls ? "✓ " : "") + option.label) {
                                    library.setNls(option.id, for: game)
                                }
                            }
                        }
                    }
                    Divider()
                    Button("Reveal in Finder") { library.revealInFinder(game: game) }
                    Button("Remove from Library") { library.remove(game: game) }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
                .menuStyle(BorderlessButtonMenuStyle())
                .frame(width: 28)
            }

            Button {
                library.launch(game: game)
            } label: {
                Text("Play").frame(maxWidth: .infinity)
            }
        }
        .padding(12)
        .background(RoundedRectangle(cornerRadius: 16).fill(Color(NSColor.controlBackgroundColor)))
        .overlay(RoundedRectangle(cornerRadius: 16).strokeBorder(Color.secondary.opacity(hovering ? 0.45 : 0.18)))
        .shadow(color: Color.black.opacity(hovering ? 0.18 : 0.06), radius: hovering ? 8 : 3, y: 2)
        .contextMenu {
            Button("Play") { library.launch(game: game) }
            if !game.nlsOptions.isEmpty {
                Menu("Text Encoding") {
                    ForEach(game.nlsOptions, id: \.id) { option in
                        Button((option.id == game.nls ? "✓ " : "") + option.label) {
                            library.setNls(option.id, for: game)
                        }
                    }
                }
            }
            Button("Reveal in Finder") { library.revealInFinder(game: game) }
            Button("Remove from Library") { library.remove(game: game) }
        }
        .help(game.rootPath)
    }

    @ViewBuilder
    private var cover: some View {
        if let image = library.loadCoverImage(game: game) {
            Image(nsImage: image)
                .resizable()
                .interpolation(game.coverKind == "icon" ? .none : .medium)
                .aspectRatio(contentMode: .fill)
        } else {
            ZStack {
                LinearGradient(
                    gradient: Gradient(colors: [Color(red: 0.20, green: 0.22, blue: 0.30), Color(red: 0.08, green: 0.09, blue: 0.13)]),
                    startPoint: .top,
                    endPoint: .bottom
                )
                Text(game.title)
                    .font(.title3)
                    .bold()
                    .foregroundColor(.white)
                    .multilineTextAlignment(.center)
                    .padding(14)
            }
        }
    }
}
