// Compile with sdk.ts and jsnir.d.ts. Each host message remounts a Dynamic whose content holds another Dynamic of
// 7000 nodes; 10 of them outnumber the node ids unless the nested mount's ids are freed with its parent's.
const n = signal(0);
onHost(() => n.update(x => x + 1));
const many = (w: number) => <Column><Text>{`nested ${w}`}</Text>{Array.from({ length: 7000 }, () => <Box />)}</Column>;
mount(() => <Column>
    <Dynamic value={n}>{v => <Column><Dynamic value={() => v}>{many}</Dynamic></Column>}</Dynamic>
</Column>);
