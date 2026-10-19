//! What the extension host says about views, source control, tasks and terminals, as events.

use std::path::PathBuf;

use rustcode_application::{
    ExtTerminalLink, WebviewOptions, WebviewSpec, NodeIcon, ScmGroup, ScmResource, ScmSource, ServiceCommand, ServiceEvent, TaskInfo, TaskRun, TerminalRequest, TreeNode,
};
use serde_json::Value;

fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

fn command(value: &Value) -> Option<ServiceCommand> {
    Some(ServiceCommand {
        command: value["command"].as_str()?.to_owned(),
        title: text(&value["title"]),
        arguments: value["arguments"].as_array().cloned().unwrap_or_default(),
    })
}

fn icon(value: &Value) -> Option<NodeIcon> {
    if let Some(file) = value["file"].as_str() {
        return Some(NodeIcon::File(PathBuf::from(file)));
    }
    Some(NodeIcon::Symbol { name: value["symbol"].as_str()?.to_owned(), color: text(&value["color"]) })
}

fn pairs(value: &Value) -> Vec<(String, String)> {
    value
        .as_array()
        .map(|list| list.iter().filter_map(|pair| Some((pair[0].as_str()?.to_owned(), pair[1].as_str()?.to_owned()))).collect())
        .unwrap_or_default()
}

fn webview_options(value: &Value) -> WebviewOptions {
    WebviewOptions {
        enable_scripts: value["enableScripts"].as_bool() == Some(true),
        enable_command_uris: value["enableCommandUris"].as_bool() == Some(true),
        local_roots: value["localResourceRoots"].as_array().map(|roots| roots.iter().filter_map(|r| r.as_str().map(PathBuf::from)).collect()),
    }
}

fn node(value: &Value) -> Option<TreeNode> {
    Some(TreeNode {
        handle: value["handle"].as_u64()?,
        label: text(&value["label"]).unwrap_or_default(),
        description: text(&value["description"]).filter(|d| !d.is_empty()),
        tooltip: text(&value["tooltip"]),
        icon: icon(&value["icon"]),
        collapsible: value["collapsible"].as_u64().unwrap_or(0).min(2) as u8,
        context_value: text(&value["contextValue"]),
        command: command(&value["command"]),
        resource: text(&value["resource"]).map(PathBuf::from),
    })
}

/// The event a message from the host stands for, or `None` when it is not about these things.
pub(crate) fn service_event(message: &Value) -> Option<ServiceEvent> {
    let view = || text(&message["view"]);
    Some(match message["type"].as_str()? {
        "treeNodes" => ServiceEvent::TreeNodes {
            request: message["request"].as_u64()?,
            view: view()?,
            parent: message["parent"].as_u64(),
            nodes: message["nodes"].as_array().map(|list| list.iter().filter_map(node).collect()).unwrap_or_default(),
        },
        "treeRefresh" => ServiceEvent::TreeRefresh { view: view()?, handle: message["handle"].as_u64() },
        "treeReady" => ServiceEvent::TreeReady { view: view()? },
        "treeInfo" => ServiceEvent::TreeInfo {
            view: view()?,
            title: text(&message["title"]),
            description: text(&message["description"]),
            message: text(&message["message"]),
            badge: message["badge"]["value"].as_u64().map(|n| (n, text(&message["badge"]["tooltip"]).unwrap_or_default())),
        },
        "treeReveal" => ServiceEvent::TreeReveal {
            view: view()?,
            path: message["path"].as_array().map(|p| p.iter().filter_map(Value::as_u64).collect()).unwrap_or_default(),
            select: message["select"].as_bool() != Some(false),
            expand: message["expand"].as_bool() == Some(true),
        },
        "scmSource" => ServiceEvent::ScmSource(ScmSource {
            id: text(&message["id"])?,
            label: text(&message["label"]).unwrap_or_default(),
            root: text(&message["root"]).map(PathBuf::from),
            count: message["count"].as_u64(),
            input_visible: message["inputVisible"].as_bool() != Some(false),
            input_placeholder: text(&message["inputPlaceholder"]).unwrap_or_default(),
            input_value: text(&message["inputValue"]).unwrap_or_default(),
            accept: command(&message["accept"]),
            commit_template: text(&message["commitTemplate"]),
            status: message["status"].as_array().map(|list| list.iter().filter_map(command).collect()).unwrap_or_default(),
            quick_diff: message["quickDiff"].as_bool() == Some(true),
        }),
        "scmGroup" => ServiceEvent::ScmGroup(ScmGroup {
            source: text(&message["source"])?,
            id: text(&message["id"])?,
            label: text(&message["label"]).unwrap_or_default(),
            hide_when_empty: message["hideWhenEmpty"].as_bool() == Some(true),
            resources: message["resources"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .filter_map(|r| {
                            Some(ScmResource {
                                index: r["index"].as_u64()? as usize,
                                path: PathBuf::from(r["path"].as_str()?),
                                tooltip: text(&r["tooltip"]),
                                strike_through: r["strikeThrough"].as_bool() == Some(true),
                                faded: r["faded"].as_bool() == Some(true),
                                context_value: text(&r["contextValue"]),
                                command: command(&r["command"]),
                                icon: icon(&r["icon"]),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }),
        "quickDiffBase" => ServiceEvent::QuickDiffBase { path: PathBuf::from(message["path"].as_str()?), text: text(&message["text"]) },
        "scmValidation" => ServiceEvent::ScmValidation {
            source: text(&message["source"])?,
            message: text(&message["message"]),
            level: message["level"].as_u64().unwrap_or(0) as u8,
        },
        "scmGroupGone" => ServiceEvent::ScmGroupGone { source: text(&message["source"])?, id: text(&message["id"])? },
        "scmGone" => ServiceEvent::ScmGone { id: text(&message["id"])? },
        "scmInput" => ServiceEvent::ScmInput { source: text(&message["source"])?, value: text(&message["value"]).unwrap_or_default() },
        "tasks" => ServiceEvent::Tasks {
            request: message["request"].as_u64()?,
            tasks: message["tasks"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .filter_map(|t| {
                            Some(TaskInfo {
                                handle: t["handle"].as_u64()?,
                                name: text(&t["name"])?,
                                source: text(&t["source"]).unwrap_or_default(),
                                group: text(&t["group"]),
                                default_group: t["defaultGroup"].as_bool() == Some(true),
                                detail: text(&t["detail"]),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        "taskRun" => ServiceEvent::RunTask(TaskRun {
            execution: message["execution"].as_u64()?,
            name: text(&message["name"]).unwrap_or_default(),
            command_line: text(&message["commandLine"])?,
            cwd: text(&message["cwd"]).map(PathBuf::from),
            env: pairs(&message["env"]),
            reveal: message["reveal"].as_bool() != Some(false),
            matchers: message["problemMatchers"].as_array().cloned().unwrap_or_default(),
        }),
        "editorCommand" => ServiceEvent::EditorCommand(command(&serde_json::json!({
            "command": message["command"],
            "arguments": message["arguments"],
        }))?),
        "taskStop" => ServiceEvent::StopTask { execution: message["execution"].as_u64()? },
        "terminalCreate" => ServiceEvent::TerminalCreate(TerminalRequest {
            id: message["id"].as_u64()?,
            name: text(&message["name"]).unwrap_or_default(),
            cwd: text(&message["cwd"]).map(PathBuf::from),
            env: pairs(&message["env"]),
            shell: text(&message["shell"]),
            shell_args: message["shellArgs"].as_array().map(|a| a.iter().filter_map(|s| text(s)).collect()).unwrap_or_default(),
            pseudo: message["pseudo"].as_bool() == Some(true),
            preserve_focus: message["preserveFocus"].as_bool() == Some(true),
        }),
        "terminalSend" => ServiceEvent::TerminalSend { id: message["id"].as_u64()?, text: text(&message["text"]).unwrap_or_default() },
        "terminalShow" => ServiceEvent::TerminalShow { id: message["id"].as_u64()?, preserve_focus: message["preserveFocus"].as_bool() == Some(true) },
        "terminalHide" => ServiceEvent::TerminalHide { id: message["id"].as_u64()? },
        "terminalClose" => ServiceEvent::TerminalClose { id: message["id"].as_u64()? },
        "webviewCreate" => ServiceEvent::WebviewCreate(WebviewSpec {
            id: message["id"].as_u64()?,
            view_type: text(&message["viewType"]).unwrap_or_default(),
            title: text(&message["title"]).unwrap_or_default(),
            column: message["column"].as_i64().unwrap_or(-1) as i32,
            preserve_focus: message["preserveFocus"].as_bool() == Some(true),
            icon: text(&message["icon"]).map(PathBuf::from),
            options: webview_options(&message["options"]),
            resource: None,
        }),
        "webviewHtml" => ServiceEvent::WebviewHtml { id: message["id"].as_u64()?, html: text(&message["html"]).unwrap_or_default() },
        "webviewTitle" => ServiceEvent::WebviewTitle { id: message["id"].as_u64()?, title: text(&message["title"]).unwrap_or_default() },
        "webviewIcon" => ServiceEvent::WebviewIcon { id: message["id"].as_u64()?, icon: text(&message["icon"]).map(PathBuf::from) },
        "webviewOptions" => ServiceEvent::WebviewOptions { id: message["id"].as_u64()?, options: webview_options(&message["options"]) },
        "webviewPost" => ServiceEvent::WebviewPost { id: message["id"].as_u64()?, message: message["message"].clone() },
        "webviewReveal" => ServiceEvent::WebviewReveal {
            id: message["id"].as_u64()?,
            column: message["column"].as_i64().map(|c| c as i32),
            preserve_focus: message["preserveFocus"].as_bool() == Some(true),
        },
        "webviewDispose" => ServiceEvent::WebviewDispose { id: message["id"].as_u64()? },
        "webviewViewInfo" => ServiceEvent::WebviewViewInfo {
            id: message["id"].as_u64()?,
            title: text(&message["title"]),
            description: text(&message["description"]),
            badge: message["badge"].as_u64(),
        },
        "customEditorDirty" => ServiceEvent::CustomEditorDirty {
            id: message["id"].as_u64()?,
            dirty: message["dirty"].as_bool() == Some(true),
            undoable: message["undoable"].as_bool() == Some(true),
        },
        "webviewSerializers" => ServiceEvent::WebviewSerializers {
            view_types: message["viewTypes"].as_array().map(|l| l.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default(),
        },
        "customEditorSaved" => ServiceEvent::CustomEditorSaved { id: message["id"].as_u64()? },
        "terminalLinkProviders" => ServiceEvent::TerminalLinkProviders { count: message["count"].as_u64().unwrap_or(0) as usize },
        "terminalLinks" => ServiceEvent::TerminalLinks {
            request: message["request"].as_u64()?,
            links: message["links"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .filter_map(|l| {
                            Some(ExtTerminalLink {
                                start: l["start"].as_u64()? as usize,
                                length: l["length"].as_u64()? as usize,
                                tooltip: text(&l["tooltip"]),
                                handle: l["handle"].as_u64()?,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        "terminalWrite" => ServiceEvent::TerminalWrite { id: message["id"].as_u64()?, data: text(&message["data"]).unwrap_or_default() },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tree_nodes_are_read_with_their_icon_and_command() {
        let event = service_event(&json!({
            "type": "treeNodes", "request": 4, "view": "deps", "parent": null,
            "nodes": [
                {"handle": 1, "label": "left", "collapsible": 1, "icon": {"symbol": "folder", "color": "charts.blue"}, "contextValue": "dir"},
                {"handle": 2, "label": "right", "description": "d", "command": {"command": "x.open", "arguments": [1, "two"]}, "resource": "/a/b"},
            ]
        }))
        .unwrap();
        let ServiceEvent::TreeNodes { request, view, parent, nodes } = event else { panic!("{event:?}") };
        assert_eq!((request, view.as_str(), parent), (4, "deps", None));
        assert_eq!(nodes[0].icon, Some(NodeIcon::Symbol { name: "folder".into(), color: Some("charts.blue".into()) }));
        assert_eq!(nodes[0].collapsible, 1);
        assert_eq!(nodes[1].command.as_ref().unwrap().arguments, vec![json!(1), json!("two")]);
        assert_eq!(nodes[1].resource, Some(PathBuf::from("/a/b")));
    }

    #[test]
    fn source_control_groups_and_resources_are_read() {
        let event = service_event(&json!({
            "type": "scmGroup", "source": "git", "id": "work", "label": "Changes", "hideWhenEmpty": true,
            "resources": [{"index": 0, "path": "/w/a.rs", "strikeThrough": true, "contextValue": "m", "command": {"command": "vscode.open"}}]
        }))
        .unwrap();
        let ServiceEvent::ScmGroup(group) = event else { panic!() };
        assert!(group.hide_when_empty);
        assert!(group.resources[0].strike_through);
        assert_eq!(group.resources[0].command.as_ref().unwrap().command, "vscode.open");
    }

    #[test]
    fn terminals_and_tasks_are_read_and_other_messages_are_not_ours() {
        let created = service_event(&json!({"type": "terminalCreate", "id": 3, "name": "Build", "pseudo": true, "env": [["A", "1"]]})).unwrap();
        assert_eq!(
            created,
            ServiceEvent::TerminalCreate(TerminalRequest {
                id: 3,
                name: "Build".into(),
                cwd: None,
                env: vec![("A".into(), "1".into())],
                shell: None,
                shell_args: vec![],
                pseudo: true,
                preserve_focus: false,
            })
        );
        let run = service_event(&json!({"type": "taskRun", "execution": 9, "name": "t", "commandLine": "ls -l"})).unwrap();
        assert!(matches!(run, ServiceEvent::RunTask(TaskRun { execution: 9, reveal: true, .. })));
        assert_eq!(service_event(&json!({"type": "statusItem"})), None);
    }
}

#[cfg(test)]
mod host_tests {
    use std::path::Path;
    use std::time::Duration;

    use rustcode_application::{ExtensionHost, HostEvent, ServiceEvent, TreeUiEvent};
    use serde_json::json;

    use crate::extension_host::{js_runtime, NodeExtensionHost};

    const MANIFEST: &str = r#"{
        "name": "svc", "publisher": "acme", "main": "./extension.js", "activationEvents": ["onWebviewPanel:svc.page"],
        "contributes": {
            "commands": [
                {"command": "svc.refresh", "title": "Refresh"}, {"command": "svc.open", "title": "Open"},
                {"command": "svc.accept", "title": "Accept"}, {"command": "svc.term", "title": "Term"},
                {"command": "svc.pty", "title": "Pty"}, {"command": "svc.title", "title": "Title"},
                {"command": "svc.panel", "title": "Panel"}
            ],
            "customEditors": [{"viewType": "svc.editor", "displayName": "Svc Editor", "selector": [{"filenamePattern": "*.SVC"}]}],
            "viewsContainers": {"activitybar": [{"id": "svc-box", "title": "Svc", "icon": "icon.svg"}]},
            "views": {"svc-box": [{"id": "svc.tree", "name": "Things", "when": "true"}, {"id": "svc.web", "name": "Web", "type": "webview"}]},
            "menus": {"view/item/context": [{"command": "svc.open", "when": "view == svc.tree && viewItem == item", "group": "inline"}]},
            "taskDefinitions": [{"type": "svc"}]
        }
    }"#;

    const CODE: &str = r#"
        const vscode = require('vscode');
        exports.activate = (context) => {
            const changed = new vscode.EventEmitter();
            const roots = [{ n: 'a', kids: ['a1', 'a2'] }, { n: 'b' }];
            const view = vscode.window.createTreeView('svc.tree', {
                treeDataProvider: {
                    onDidChangeTreeData: changed.event,
                    getChildren: (e) => (e ? (e.kids || []).map((k) => ({ n: k, parent: e })) : roots),
                    getTreeItem: (e) => {
                        const item = new vscode.TreeItem(e.n, e.kids ? vscode.TreeItemCollapsibleState.Collapsed : 0);
                        item.contextValue = 'item';
                        item.description = 'd-' + e.n;
                        item.iconPath = new vscode.ThemeIcon('file', new vscode.ThemeColor('charts.blue'));
                        item.command = { command: 'svc.open', title: 'Open', arguments: [e] };
                        return item;
                    },
                    getParent: (e) => e.parent,
                },
            });
            view.onDidChangeSelection((e) => vscode.window.showInformationMessage('selected ' + e.selection.map((s) => s.n).join(',')));
            view.onDidExpandElement((e) => vscode.window.showInformationMessage('expanded ' + e.element.n));
            context.subscriptions.push(
                vscode.commands.registerCommand('svc.refresh', () => changed.fire()),
                vscode.commands.registerCommand('svc.open', (e) => vscode.window.showInformationMessage('open ' + (e && e.n))),
                vscode.commands.registerCommand('svc.title', () => { view.title = 'Renamed'; view.message = 'hello'; }),
            );
            const folder = vscode.workspace.workspaceFolders[0].uri.fsPath;
            const sc = vscode.scm.createSourceControl('svc', 'Svc', vscode.workspace.workspaceFolders[0].uri);
            sc.inputBox.placeholder = 'Message';
            sc.count = 2;
            sc.acceptInputCommand = { command: 'svc.accept', title: 'Accept' };
            sc.quickDiffProvider = { provideOriginalResource: (uri) => vscode.Uri.file(folder + '/original.txt') };
            sc.statusBarCommands = [{ command: 'svc.open', title: '$(sync) Sync' }];
            sc.inputBox.validateInput = (value) => (value.length > 5 ? undefined : { message: 'too short', type: 2 });
            const group = sc.createResourceGroup('work', 'Changes');
            group.resourceStates = [{ resourceUri: vscode.Uri.file(folder + '/a.txt'), decorations: { strikeThrough: true, tooltip: 'gone' }, contextValue: 'del', command: { command: 'svc.open', title: 'Open', arguments: [] } }];
            context.subscriptions.push(vscode.commands.registerCommand('svc.accept', () => vscode.window.showInformationMessage('accepted ' + sc.inputBox.value)));

            vscode.tasks.registerTaskProvider('svc', {
                provideTasks: () => {
                    const task = new vscode.Task({ type: 'svc' }, vscode.TaskScope.Workspace, 'echo task', 'svc', new vscode.ShellExecution('echo', ['hi there']));
                    task.group = vscode.TaskGroup.Build;
                    return [task];
                },
            });
            vscode.window.onDidCloseTerminal((t) => vscode.window.showInformationMessage('closed ' + t.name));
            context.subscriptions.push(vscode.commands.registerCommand('svc.panel', () => {
                const panel = vscode.window.createWebviewPanel('svc.page', 'Hello', vscode.ViewColumn.Beside, { enableScripts: true, localResourceRoots: [vscode.Uri.file(folder)] });
                panel.webview.html = '<h1>' + panel.webview.asWebviewUri(vscode.Uri.file(folder + '/a b.js')).toString() + '</h1>';
                panel.webview.onDidReceiveMessage((m) => {
                    if (m.bytes) {
                        const total = Array.from(m.bytes).reduce((a, b) => a + b, 0);
                        panel.webview.postMessage({ total, kind: m.bytes.constructor.name, back: new Uint8Array([9, 8]) });
                        return;
                    }
                    panel.webview.postMessage({ echo: m });
                    panel.title = 'Got ' + m.n;
                });
                panel.onDidDispose(() => vscode.window.showInformationMessage('page closed'));
            }));
            vscode.window.registerWebviewViewProvider('svc.web', {
                resolveWebviewView: (view) => {
                    view.webview.options = { enableScripts: true };
                    view.webview.html = 'side ' + view.viewType;
                    view.title = 'Side Page';
                    view.badge = { value: 3, tooltip: 'three' };
                },
            });
            vscode.window.registerWebviewPanelSerializer('svc.page', {
                deserializeWebviewPanel: async (panel, state) => {
                    panel.webview.html = 'restored ' + JSON.stringify(state) + ' ' + panel.title;
                },
            });
            const edited = new vscode.EventEmitter();
            vscode.window.registerCustomEditorProvider('svc.editor', {
                onDidChangeCustomDocument: edited.event,
                openCustomDocument: (uri) => ({ uri, dispose() {} }),
                resolveCustomEditor: (document, panel) => {
                    edited.fire({ document, undo: () => vscode.window.showInformationMessage('undone'), redo: () => vscode.window.showInformationMessage('redone') });
                    panel.webview.options = { enableScripts: true };
                    panel.webview.html = 'custom ' + document.uri.fsPath;
                },
                saveCustomDocument: async (document) => vscode.window.showInformationMessage('saved ' + document.uri.fsPath),
            });
            vscode.window.registerTerminalLinkProvider({
                provideTerminalLinks: (ctx) => {
                    const i = ctx.line.indexOf('TICKET-');
                    return i < 0 ? [] : [{ startIndex: i, length: 10, tooltip: 'Open ticket', data: ctx.line.slice(i, i + 10), terminal: ctx.terminal.name }];
                },
                handleTerminalLink: (link) => vscode.window.showInformationMessage('link ' + link.data + ' in ' + link.terminal),
            });
            vscode.window.onDidChangeTerminalShellIntegration((e) => {
                if (e.shellIntegration.cwd) vscode.window.showInformationMessage('cwd ' + e.shellIntegration.cwd.fsPath);
            });
            vscode.window.onDidStartTerminalShellExecution((e) => vscode.window.showInformationMessage('start ' + e.execution.commandLine.value + ' trusted=' + e.execution.commandLine.isTrusted));
            vscode.window.onDidEndTerminalShellExecution(async (e) => {
                let out = '';
                for await (const chunk of e.execution.read()) out += chunk;
                vscode.window.showInformationMessage('end ' + e.exitCode + ' ' + JSON.stringify(out));
            });
            context.subscriptions.push(
                vscode.commands.registerCommand('svc.term', () => {
                    const t = vscode.window.createTerminal({ name: 'Plain', cwd: folder });
                    t.sendText('echo x');
                    vscode.window.showInformationMessage('terminals ' + vscode.window.terminals.length);
                }),
                vscode.commands.registerCommand('svc.pty', () => {
                    const write = new vscode.EventEmitter();
                    vscode.window.createTerminal({
                        name: 'Drawn',
                        pty: { onDidWrite: write.event, open: (d) => write.fire('hello ' + d.columns + '\r\n'), close() {}, handleInput: (data) => write.fire('got ' + data) },
                    });
                }),
            );
        };
    "#;

    fn collect(host: &NodeExtensionHost, done: impl Fn(&[ServiceEvent]) -> bool) -> Vec<ServiceEvent> {
        let mut all = Vec::new();
        for _ in 0..300 {
            all.extend(host.take_service_events());
            if done(&all) {
                return all;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("events did not come: {all:?}");
    }

    fn infos(host: &NodeExtensionHost, count: usize) -> Vec<String> {
        let mut found = Vec::new();
        for _ in 0..300 {
            found.extend(host.take_events().into_iter().filter_map(|e| match e {
                HostEvent::Info(text) => Some(text),
                _ => None,
            }));
            if found.len() >= count {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        found
    }

    fn make_host(name: &str) -> (NodeExtensionHost, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("rustcode-svc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("svc-1.0.0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("package.json"), MANIFEST).unwrap();
        std::fs::write(dir.join("extension.js"), CODE).unwrap();
        let root = root.canonicalize().unwrap();
        (NodeExtensionHost::new(&[root.clone()]), root)
    }

    #[test]
    fn a_tree_view_starts_its_extension_and_serves_its_elements() {
        if js_runtime().is_none() {
            return;
        }
        let (host, root) = make_host("tree");
        let folders = [root.clone()];
        let request = host.tree_children("svc.tree", None, &folders).expect("the view belongs to an extension");
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::TreeNodes { request: r, .. } if *r == request)));
        let nodes = events
            .iter()
            .find_map(|e| match e {
                ServiceEvent::TreeNodes { nodes, .. } => Some(nodes.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(nodes.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!((nodes[0].collapsible, nodes[1].collapsible), (1, 0));
        assert_eq!(nodes[0].description.as_deref(), Some("d-a"));
        assert_eq!(nodes[0].context_value.as_deref(), Some("item"));
        assert!(matches!(nodes[0].icon, Some(rustcode_application::NodeIcon::Symbol { ref name, ref color }) if name == "file" && color.as_deref() == Some("charts.blue")));

        // Children of `a`, then a click: the element comes back to the command that handles it.
        let request = host.tree_children("svc.tree", Some(nodes[0].handle), &folders).unwrap();
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::TreeNodes { request: r, .. } if *r == request)));
        let kids: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                ServiceEvent::TreeNodes { nodes, request: r, .. } if *r == request => Some(nodes.iter().map(|n| n.label.clone()).collect::<Vec<_>>()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(kids, ["a1", "a2"]);
        host.run_command("svc.open", &[json!({"$treeElement": {"view": "svc.tree", "handle": nodes[1].handle}})], &folders);
        assert_eq!(infos(&host, 1), ["open b"]);

        // What the user did is heard by the extension; changes of data and of the title reach the app.
        host.tree_event("svc.tree", TreeUiEvent::Selected(vec![nodes[0].handle]));
        host.tree_event("svc.tree", TreeUiEvent::Expanded(nodes[0].handle));
        assert_eq!(infos(&host, 2), ["selected a", "expanded a"]);
        host.execute("svc.refresh", &folders);
        host.execute("svc.title", &folders);
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::TreeInfo { .. })));
        assert!(events.contains(&ServiceEvent::TreeRefresh { view: "svc.tree".into(), handle: None }), "{events:?}");
        assert!(events.iter().any(|e| matches!(e, ServiceEvent::TreeInfo { title: Some(t), message: Some(m), .. } if t == "Renamed" && m == "hello")), "{events:?}");
        // A view nobody contributes is not served.
        assert_eq!(host.tree_children("other.view", None, &folders), None);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_source_control_provider_shows_its_groups_and_hears_its_input() {
        if js_runtime().is_none() {
            return;
        }
        let (host, root) = make_host("scm");
        let folders = [root.clone()];
        // The extension starts with the first look at its view.
        host.tree_children("svc.tree", None, &folders).unwrap();
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::ScmGroup(_))) && e.iter().any(|e| matches!(e, ServiceEvent::ScmSource(_))));
        let source = events.iter().rev().find_map(|e| if let ServiceEvent::ScmSource(s) = e { Some(s.clone()) } else { None }).unwrap();
        assert_eq!((source.id.as_str(), source.label.as_str(), source.count), ("svc", "Svc", Some(2)));
        assert_eq!(source.input_placeholder, "Message");
        assert_eq!(source.accept.as_ref().map(|c| c.command.as_str()), Some("svc.accept"));
        let group = events.iter().find_map(|e| if let ServiceEvent::ScmGroup(g) = e { Some(g.clone()) } else { None }).unwrap();
        assert_eq!((group.id.as_str(), group.label.as_str()), ("work", "Changes"));
        assert_eq!(group.resources.len(), 1);
        let file = &group.resources[0];
        assert!(file.strike_through && file.path.ends_with("a.txt") && file.tooltip.as_deref() == Some("gone"));
        assert_eq!(file.context_value.as_deref(), Some("del"));

        host.scm_input("svc", "my message");
        host.run_command("svc.accept", &[], &folders);
        assert_eq!(infos(&host, 1), ["accepted my message"]);

        // The provider's verdict on the text, the status bar commands, and the committed text of a file.
        assert!(source.quick_diff);
        assert_eq!(source.status.len(), 1);
        assert_eq!(source.status[0].title.as_deref(), Some("$(sync) Sync"));
        host.scm_input("svc", "abc");
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::ScmValidation { message: Some(_), .. })));
        assert!(events.contains(&ServiceEvent::ScmValidation { source: "svc".into(), message: Some("too short".into()), level: 2 }), "{events:?}");
        host.scm_input("svc", "long enough");
        let _ = host.take_service_events();
        host.scm_input("svc", "long enough");
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::ScmValidation { message: None, .. })));
        assert!(!events.is_empty());
        std::fs::write(root.join("original.txt"), "the committed text\n").unwrap();
        host.quick_diff_base(&root.join("a.txt"));
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::QuickDiffBase { .. })));
        assert!(events.contains(&ServiceEvent::QuickDiffBase { path: root.join("a.txt"), text: Some("the committed text\n".into()) }), "{events:?}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn tasks_are_listed_and_run_as_command_lines() {
        if js_runtime().is_none() {
            return;
        }
        let (host, root) = make_host("tasks");
        let folders = [root.clone()];
        let request = host.fetch_tasks(&folders).expect("an extension defines a task type");
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::Tasks { request: r, .. } if *r == request)));
        let tasks = events.iter().find_map(|e| if let ServiceEvent::Tasks { tasks, .. } = e { Some(tasks.clone()) } else { None }).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!((tasks[0].name.as_str(), tasks[0].source.as_str(), tasks[0].group.as_deref()), ("echo task", "svc", Some("build")));
        host.execute_task(tasks[0].handle);
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::RunTask(_))));
        let run = events.iter().find_map(|e| if let ServiceEvent::RunTask(r) = e { Some(r.clone()) } else { None }).unwrap();
        assert_eq!(run.command_line, "echo 'hi there'");
        assert_eq!(run.name, "echo task");
        host.task_ended(run.execution, Some(0));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn terminals_open_get_text_and_close_and_drawn_ones_talk_both_ways() {
        if js_runtime().is_none() {
            return;
        }
        let (host, root) = make_host("terminals");
        let folders = [root.clone()];
        host.tree_children("svc.tree", None, &folders).unwrap();
        let _ = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::ScmSource(_))));
        host.execute("svc.term", &folders);
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::TerminalSend { .. })));
        let created = events.iter().find_map(|e| if let ServiceEvent::TerminalCreate(t) = e { Some(t.clone()) } else { None }).unwrap();
        assert_eq!((created.name.as_str(), created.pseudo), ("Plain", false));
        assert_eq!(created.cwd.as_deref(), Some(root.as_path()));
        assert!(events.contains(&ServiceEvent::TerminalSend { id: created.id, text: "echo x\r".into() }), "{events:?}");
        assert_eq!(infos(&host, 1), ["terminals 1"]);
        host.terminal_closed(created.id, Some(0));
        assert_eq!(infos(&host, 1), ["closed Plain"]);

        // A terminal the extension draws: its text comes as writes, what is typed goes to it.
        host.execute("svc.pty", &folders);
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::TerminalCreate(t) if t.pseudo)));
        let drawn = events.iter().find_map(|e| if let ServiceEvent::TerminalCreate(t) = e { Some(t.clone()) } else { None }).unwrap();
        host.terminal_opened(drawn.id, None, 100, 30);
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::TerminalWrite { .. })));
        assert!(events.contains(&ServiceEvent::TerminalWrite { id: drawn.id, data: "hello 100\r\n".into() }), "{events:?}");
        host.terminal_input(drawn.id, "k");
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::TerminalWrite { data, .. } if data == "got k")));
        assert!(!events.is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn webview_panels_and_custom_editors_talk_both_ways() {
        if js_runtime().is_none() {
            return;
        }
        let (host, root) = make_host("webview");
        let folders = [root.clone()];
        host.tree_children("svc.tree", None, &folders).unwrap();
        let _ = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::ScmSource(_))));
        host.execute("svc.panel", &folders);
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::WebviewHtml { .. })));
        let spec = events.iter().find_map(|e| if let ServiceEvent::WebviewCreate(s) = e { Some(s.clone()) } else { None }).unwrap();
        assert_eq!((spec.view_type.as_str(), spec.title.as_str(), spec.column), ("svc.page", "Hello", -2));
        assert!(spec.options.enable_scripts && !spec.options.enable_command_uris);
        assert_eq!(spec.options.local_roots, Some(vec![root.clone()]));
        let html = events.iter().find_map(|e| if let ServiceEvent::WebviewHtml { html, .. } = e { Some(html.clone()) } else { None }).unwrap();
        let origin = if cfg!(windows) { "http://rustcode-resource.localhost" } else { "rustcode-resource://localhost" };
        assert_eq!(html, format!("<h1>{origin}{}/a%20b.js</h1>", root.display()));

        // The page speaks: the extension answers with a message and a new title.
        host.webview_message(spec.id, &json!({"n": 7}));
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::WebviewTitle { .. })) && e.iter().any(|e| matches!(e, ServiceEvent::WebviewPost { .. })));
        assert!(events.contains(&ServiceEvent::WebviewPost { id: spec.id, message: json!({"echo": {"n": 7}}) }), "{events:?}");
        assert!(events.contains(&ServiceEvent::WebviewTitle { id: spec.id, title: "Got 7".into() }), "{events:?}");
        // Typed arrays go through as bytes, both ways.
        host.webview_message(spec.id, &json!({"bytes": {"$bin": "Uint8Array", "data": "BAUG"}}));
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::WebviewPost { message, .. } if message["total"] == 15)));
        assert!(
            events.contains(&ServiceEvent::WebviewPost {
                id: spec.id,
                message: json!({"total": 15, "kind": "Uint8Array", "back": {"$bin": "Uint8Array", "data": "CQg="}})
            }),
            "{events:?}"
        );
        host.webview_closed(spec.id);
        assert_eq!(infos(&host, 1), ["page closed"]);

        // A file opened with a custom editor: the extension fills the tab RustCode made, and saves.
        let file = root.join("x.svc");
        host.custom_editor_open(77, "svc.editor", &file, &folders);
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::WebviewHtml { id: 77, .. })));
        let custom_events = events.clone();
        assert!(events.contains(&ServiceEvent::WebviewHtml { id: 77, html: format!("custom {}", file.display()) }), "{events:?}");
        assert!(events.iter().any(|e| matches!(e, ServiceEvent::WebviewOptions { id: 77, options } if options.enable_scripts)), "{events:?}");
        // A view of the side bar drawn by a page: the extension fills the page RustCode made for it.
        host.webview_view_open(55, "svc.web", &folders);
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::WebviewViewInfo { badge: Some(3), .. })));
        assert!(events.contains(&ServiceEvent::WebviewHtml { id: 55, html: "side svc.web".into() }), "{events:?}");
        assert!(events.iter().any(|e| matches!(e, ServiceEvent::WebviewViewInfo { id: 55, title: Some(t), .. } if t == "Side Page")), "{events:?}");
        // The extension keeps a history: it says so, and undo and redo reach it.
        assert!(custom_events.contains(&ServiceEvent::CustomEditorDirty { id: 77, dirty: true, undoable: true }), "{custom_events:?}");
        host.custom_editor_history(77, false);
        host.custom_editor_history(77, true);
        assert_eq!(infos(&host, 2), ["undone", "redone"]);

        // A panel of the last run is made again from the state the page kept.
        host.webview_restore(90, "svc.page", "Old page", &json!({"n": 3}), &folders);
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::WebviewHtml { id: 90, .. })));
        assert!(events.contains(&ServiceEvent::WebviewHtml { id: 90, html: "restored {\"n\":3} Old page".into() }), "{events:?}");
        host.custom_editor_save(77, None);
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::CustomEditorSaved { id: 77 })));
        assert!(!events.is_empty());
        assert_eq!(infos(&host, 1), [format!("saved {}", file.display())]);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn terminal_links_come_from_providers_and_shell_events_reach_extensions() {
        use rustcode_application::{ShellEvent, USER_TERMINAL_BASE};
        if js_runtime().is_none() {
            return;
        }
        let (host, root) = make_host("shell");
        let folders = [root.clone()];
        host.tree_children("svc.tree", None, &folders).unwrap();
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::TerminalLinkProviders { .. })));
        assert!(events.contains(&ServiceEvent::TerminalLinkProviders { count: 1 }), "{events:?}");

        // A line of a terminal the user opened: the provider marks a ticket.
        let id = USER_TERMINAL_BASE + 2;
        let request = host.terminal_links(id, "shell", "fixed in TICKET-123 today").expect("the host runs");
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::TerminalLinks { request: r, .. } if *r == request)));
        let links = events.iter().find_map(|e| if let ServiceEvent::TerminalLinks { links, .. } = e { Some(links.clone()) } else { None }).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!((links[0].start, links[0].length, links[0].tooltip.as_deref()), (9, 10, Some("Open ticket")));
        host.terminal_link_activate(links[0].handle);
        assert_eq!(infos(&host, 1), ["link TICKET-123 in shell"]);
        let request = host.terminal_links(id, "shell", "nothing here").unwrap();
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, ServiceEvent::TerminalLinks { request: r, .. } if *r == request)));
        assert!(events.iter().any(|e| matches!(e, ServiceEvent::TerminalLinks { links, .. } if links.is_empty())), "{events:?}");

        // A command in that shell: the extension hears where it was, what ran, what it printed and how it ended.
        host.terminal_shell(id, "shell", ShellEvent::Integrated);
        host.terminal_shell(id, "shell", ShellEvent::Cwd("/work".into()));
        host.terminal_shell(id, "shell", ShellEvent::CommandLine { text: "echo hi".into(), trusted: true });
        host.terminal_shell(id, "shell", ShellEvent::Started);
        host.terminal_shell(id, "shell", ShellEvent::Output(b"hi\r\n".to_vec()));
        host.terminal_shell(id, "shell", ShellEvent::Ended { code: Some(0) });
        assert_eq!(infos(&host, 3), ["cwd /work", "start echo hi trusted=true", "end 0 \"hi\\r\\n\""]);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn views_containers_and_menus_come_from_the_manifest() {
        let (_, root) = make_host("manifest");
        let found = crate::contributions::read_contributions(&root.join("svc-1.0.0"));
        assert_eq!(found.workbench.containers.len(), 1);
        assert_eq!(found.workbench.containers[0].id, "svc-box");
        assert_eq!(found.workbench.views[0].id, "svc.tree");
        assert_eq!(found.workbench.views[0].container, "svc-box");
        assert!(!found.workbench.views[0].webview && found.workbench.views[1].webview);
        assert_eq!(found.workbench.menus[0].menu, "view/item/context");
        let editor = &found.workbench.custom_editors[0];
        assert_eq!((editor.view_type.as_str(), editor.default_priority), ("svc.editor", true));
        assert!(editor.matches("Notes.svc") && !editor.matches("notes.txt"));
        assert_eq!(found.workbench.menus[0].when.as_deref(), Some("view == svc.tree && viewItem == item"));
        let _ = Path::new("");
        std::fs::remove_dir_all(root).ok();
    }
}
