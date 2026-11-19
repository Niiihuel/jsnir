// Port of the React Bits Stepper (MIT + Commons Clause, © 2026 David Haz) with its "onboarding" demo,
// https://github.com/DavidHDev/react-bits/tree/fefc5b05ca34a7475b38a977127441fbfad2f177/src/content/Components/Stepper
// following comparison/stepper/SPEC.md. Styles come from Stepper.css and the dark theme in Stepper.jsx.
// JS only decides what changes; Rust lays out, draws, runs hover/press states and every transition.

// ---------- Theme (dark) ----------
const frame = 0x26232ea8; // rgba(38, 35, 46, 0.66)
const frameEdge = 0xffffff14;
const frameHighlight = 0xffffff12;
const tile: [number, number] = [0x3a3644ff, 0x27242fff];
const tileEdge = 0xffffff12;
const tileHighlight = 0xffffff1f;
const tileShadow: Shadow = { y: 4, blur: 10, spread: -4, color: 0x000000b3 };
const well = 0xffffff12;
const ink = 0xf4f4f5ff;
const muted = 0xf4f4f58c;
const faint = 0xffffff38;
const ring = 0xf4f4f54d;
const solid = 0xf4f4f5ff;
const solidInk = 0x18181bff;
const hoverFill = 0xffffff0a;

const radius = 24;
const wellRadius = Math.max(6, radius - 8);
const tileRadius = Math.max(4, wellRadius - 4);
const total = 4;

// ---------- Curves ----------
const settle: [number, number, number, number] = [0.22, 1, 0.36, 1];
function ease(property: Property, duration: number): Transition {
    return { property: property, duration: duration, easing: "ease" };
}
function spring(property: Property, duration: number): Transition {
    return { property: property, duration: duration, easing: "spring" };
}
function bezier(property: Property, duration: number, points: [number, number, number, number]): Transition {
    return { property: property, duration: duration, easing: "bezier", bezier: points };
}

// ---------- Text styles ----------
const headingFont: Font = { size: 17, weight: 600, lineHeight: 1.3, letterSpacing: -0.01 };
const copyFont: Font = { size: 13.5, lineHeight: 1.55 };

function Heading(props: { children: Source<string> }): Element {
    return <Text font={headingFont} paint={{ color: ink }}>{props.children}</Text>;
}
function Copy(props: { top?: number; children: Source<string> }): Element {
    return (
        <Text style={{ margin: [props.top ?? 6, 0, 0, 0] }} font={copyFont} paint={{ color: muted }}>
            {props.children}
        </Text>
    );
}

// ---------- Demo state ----------
interface Args {
    step: number;
    name: string;
    plan: "pro" | "starter";
}
const start = args<Args>();
const name = signal(start.name ?? "");
const plan = signal(start.plan ?? "pro");
const current = signal(Math.min(Math.max(Math.round(start.step ?? 1), 1), total + 1));
const trimmedName = () => name.value.trim();

// ---------- Demo content (StepperDemo.jsx) ----------

/** A selectable card, as `Option` in the demo. */
function Option(props: { title: string; detail: string; selected: () => boolean; onSelect: () => void }): Element {
    return (
        <Row
            style={{ grow: 1, basis: 0, align: "center", justify: "between", gap: 12, padding: [11, 14, 11, 14] }}
            onPress={props.onSelect}
            paint={() => {
                const selected = props.selected();
                return {
                    radius: 12,
                    background: selected ? undefined : well,
                    gradient: selected ? tile : undefined,
                    edge: selected ? ring : tileEdge,
                    shadows: selected ? [tileShadow] : [],
                    transitions: [ease("background", 200), ease("edge", 200)],
                };
            }}
        >
            <Box style={{ gap: 2 }}>
                <Text font={{ size: 14, weight: 600 }} paint={{ color: ink }}>{props.title}</Text>
                <Text font={{ size: 12.5 }} paint={{ color: muted }}>{props.detail}</Text>
            </Box>
        </Row>
    );
}

function Welcome(): Element {
    return (
        <Box>
            <Heading>Welcome aboard</Heading>
            <Copy>Set up your workspace in a few quick steps. It only takes a minute.</Copy>
        </Box>
    );
}
function Plan(): Element {
    return (
        <Box>
            <Heading>Choose a plan</Heading>
            <Copy>You can change this at any time.</Copy>
            <Row style={{ gap: 8, margin: [14, 0, 0, 0] }}>
                <Option title="Starter" detail="Free forever" selected={() => plan.value === "starter"} onSelect={() => (plan.value = "starter")} />
                <Option title="Pro" detail="$12 per month" selected={() => plan.value === "pro"} onSelect={() => (plan.value = "pro")} />
            </Row>
        </Box>
    );
}
function Profile(): Element {
    return (
        <Box>
            <Heading>What should we call you?</Heading>
            <TextInput
                value={name.value}
                placeholder="Your name"
                style={{ height: 42, margin: [14, 0, 0, 0], padding: [0, 14, 0, 14] }}
                font={{ size: 14 }}
                paint={{ radius: 12, background: well, edge: tileEdge, color: ink, focus: { edge: ring }, transitions: [ease("edge", 200)] }}
                onChange={(text) => (name.value = text)}
            />
        </Box>
    );
}
function Review(): Element {
    return (
        <Box>
            <Heading>{() => "Looking good" + (trimmedName() ? ", " + trimmedName() : "")}</Heading>
            <Copy>{() => "You picked the " + (plan.value === "pro" ? "Pro" : "Starter") + " plan. Complete setup to open your workspace."}</Copy>
        </Box>
    );
}
function Done(): Element {
    return (
        <Row style={{ align: "center", gap: 14, padding: [4, 0, 4, 0] }}>
            <Box style={{ width: 40, height: 40, shrink: 0, align: "center", justify: "center" }} paint={{ radius: 12, background: well }}>
                <Icon name="checkCircle" size={20} stroke={1.8} paint={{ color: ink }} />
            </Box>
            <Box>
                <Heading>Workspace ready</Heading>
                <Copy top={2}>{() => "Welcome in" + (trimmedName() ? ", " + trimmedName() : "") + "."}</Copy>
            </Box>
        </Row>
    );
}
const titles = ["Welcome", "Plan", "Profile", "Review"];
function content(step: number): Element {
    return [Welcome, Plan, Profile, Review, Done][step - 1]();
}

// ---------- Stepper ----------

function go(target: number): void {
    const next = Math.min(Math.max(target, 1), total + 1);
    if (next !== current.value) current.value = next;
}
function status(number: number): "complete" | "active" | "upcoming" {
    const step = current.value;
    return step > total || number < step ? "complete" : number === step ? "active" : "upcoming";
}

function Badge(props: { number: number }): Element {
    return (
        <Box
            style={{ width: 22, height: 22, shrink: 0, align: "center", justify: "center" }}
            paint={() => {
                const s = status(props.number);
                return {
                    radius: 11,
                    background: s === "complete" ? solid : undefined,
                    edge: s === "complete" ? undefined : s === "active" ? ink : faint,
                    edgeWidth: s === "active" ? 1.5 : 1,
                    transitions: [ease("background", 150), ease("edge", 150)],
                };
            }}
        >
            <Dynamic value={() => status(props.number) === "complete"}>
                {(complete) =>
                    complete ? (
                        <Icon
                            name="tick"
                            size={16}
                            stroke={2.4}
                            paint={{ color: solidInk, starting: { draw: 0 }, transitions: [bezier("draw", 360, settle)] }}
                        />
                    ) : (
                        <Text
                            font={{ size: 11.5, weight: 600, lineHeight: 1 }}
                            paint={() => ({ color: status(props.number) === "active" ? ink : muted, transitions: [ease("color", 150)] })}
                        >
                            {String(props.number)}
                        </Text>
                    )
                }
            </Dynamic>
        </Box>
    );
}

function Segment(props: { number: number; ref: (id: number) => void }): Element {
    const number = props.number;
    return (
        <Row
            ref={props.ref}
            style={{ grow: 1, height: 40, align: "center", justify: "center", gap: 8, padding: [0, 10, 0, 10] }}
            disabled={() => number === current.value}
            onPress={() => go(number)}
            paint={{ radius: tileRadius, hover: { background: hoverFill }, transitions: [ease("background", 200)] }}
        >
            <Badge number={number} />
            <Text
                font={{ size: 13, weight: 600, lineHeight: 1.2 }}
                paint={() => ({ color: status(number) === "upcoming" ? muted : ink, transitions: [ease("color", 200)] })}
            >
                {titles[number - 1]}
            </Text>
        </Row>
    );
}

function Header(): Element {
    const segments: number[] = [];
    return (
        <Row style={{ gap: 4, padding: 4 }} paint={{ radius: wellRadius, background: well }}>
            <Box
                style={{ position: "absolute", left: 0, top: 0 }}
                ref={(glider) => {
                    // The segments mount after the glider, which must paint below them: follow them once they exist.
                    const scope = currentScope;
                    mountLater.push(() =>
                        inScope(scope, () =>
                            effect(() => {
                                const follow = segments[Math.min(current.value, total) - 1];
                                send([{ op: "setstyle", id: glider, style: { position: "absolute", left: 0, top: 0, follow: follow } }]);
                            }),
                        ),
                    );
                }}
                paint={() => ({
                    radius: tileRadius,
                    gradient: tile,
                    edge: tileEdge,
                    highlight: tileHighlight,
                    shadows: [tileShadow],
                    opacity: current.value > total ? 0 : 1,
                    transitions: [spring("left", 560), spring("width", 560), ease("opacity", 200)],
                })}
            />
            {[1, 2, 3, 4].map((number) => (
                <Segment number={number} ref={(id) => (segments[number - 1] = id)} />
            ))}
        </Row>
    );
}

/** The step content. Changing step slides the new pane in while the old one fades out on top. */
function Viewport(): Element {
    let pane: Mounted | null = null;
    let shown = current.value;
    const paneStyle: Style = { padding: [6, 20, 6, 20] };
    return (
        <Box
            style={{ margin: [-6, -20, -6, -20], clip: true }}
            paint={{ transitions: [spring("height", 560)] }}
            ref={(viewport) => {
                const scope = currentScope;
                const show = (step: number, direction: number) => {
                    inScope(scope, () => {
                        pane = mountInto(
                            viewport,
                            <Box
                                style={paneStyle}
                                paint={{
                                    starting: direction === 0 ? undefined : { x: direction * 36, opacity: 0 },
                                    transitions: [spring("x", 560), { property: "opacity", duration: 420, easing: "easeOut", hold: 0.25 }],
                                }}
                            >
                                {content(step)}
                            </Box>,
                        );
                    });
                };
                mountLater.push(() => show(shown, 0));
                effect(() => {
                    const step = current.value;
                    if (step === shown || pane === null) return;
                    const direction = step > shown ? 1 : -1;
                    const leaving = pane;
                    // The leaving pane floats on top, fades and slides out, then goes away.
                    send([{ op: "setstyle", id: leaving.id, style: { ...paneStyle, position: "absolute", left: 0, top: 0, width: 440 } }]);
                    setPaint(leaving.id, {
                        opacity: 0,
                        x: -direction * 36,
                        transitions: [bezier("opacity", 140, [0.2, 0, 0.4, 1]), bezier("x", 200, [0.4, 0, 1, 1])],
                    });
                    after(200, () => leaving.remove());
                    shown = step;
                    show(step, direction);
                });
            }}
        />
    );
}

function BackButton(): Element {
    return (
        <Row
            style={{ height: 38, align: "center", gap: 6, padding: [0, 14, 0, 10], shrink: 0 }}
            disabled={() => current.value <= 1}
            onPress={() => go(current.value - 1)}
            paint={() => ({
                radius: 12,
                gradient: tile,
                edge: tileEdge,
                highlight: tileHighlight,
                shadows: [tileShadow],
                opacity: current.value <= 1 ? 0 : 1,
                hover: { brightness: 1.12 },
                active: { brightness: 0.85, scale: 0.96 },
                transitions: [ease("opacity", 200), ease("brightness", 200), ease("scale", 200)],
            })}
        >
            <Icon name="arrowLeft" size={16} stroke={2} paint={{ color: ink }} />
            <Text font={{ size: 13.5, weight: 600, lineHeight: 1 }} paint={{ color: ink }}>Back</Text>
        </Row>
    );
}

function NextButton(): Element {
    const needsName = () => current.value === 3 && trimmedName() === "";
    const isLast = () => current.value === total;
    return (
        <Row
            style={{ height: 38, align: "center", gap: 6, padding: [0, 11, 0, 14], shrink: 0 }}
            disabled={needsName}
            onPress={() => go(current.value + 1)}
            paint={() => ({
                radius: 12,
                background: solid,
                shadows: [{ y: 4, blur: 12, spread: -6, color: 0x00000080 }],
                opacity: needsName() ? 0.4 : 1,
                hover: { brightness: 0.9 },
                active: { brightness: 0.85, scale: 0.96 },
                transitions: [ease("opacity", 200), ease("brightness", 200), ease("scale", 200)],
            })}
        >
            <Text font={{ size: 13.5, weight: 600, lineHeight: 1 }} paint={{ color: solidInk }}>
                {() => (isLast() ? "Complete" : "Continue")}
            </Text>
            <Dynamic value={isLast}>
                {(last) => <Icon name={last ? "tick" : "arrowRight"} size={16} stroke={2} paint={{ color: solidInk }} />}
            </Dynamic>
        </Row>
    );
}

function Footer(): Element {
    const completed = () => current.value > total;
    return (
        <Box
            style={() => ({ clip: true, height: completed() ? 0 : undefined })}
            paint={() => ({ opacity: completed() ? 0 : 1, transitions: [spring("height", 480), ease("opacity", 200)] })}
        >
            <Row style={{ align: "center", justify: "between", gap: 12, padding: [20, 0, 0, 0] }}>
                <BackButton />
                <NextButton />
            </Row>
        </Box>
    );
}

function Stepper(): Element {
    let self = 0;
    return (
        <Box
            ref={(id) => (self = id)}
            style={{ position: "absolute", left: 100, top: 48, width: 440, padding: 8 }}
            paint={{
                radius: radius,
                background: frame,
                shadows: [
                    { y: 22, blur: 44, spread: -20, color: 0x000000cc },
                    { y: 6, blur: 16, spread: -8, color: 0x00000080 },
                ],
            }}
        >
            <Header />
            <Box style={{ padding: [20, 12, 12, 12] }}>
                <Viewport />
                <Footer />
            </Box>
            {/* The rim draws the frame's inner edge on top of everything. */}
            <Box style={() => ({ position: "absolute", left: 0, top: 0, follow: self })} paint={{ radius: radius, edge: frameEdge, highlight: frameHighlight }} />
        </Box>
    );
}

function Page(): Element {
    return (
        <Box paint={{ background: 0x0b0b0fff }}>
            <Stepper />
        </Box>
    );
}
mount(Page);
