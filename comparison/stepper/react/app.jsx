// Reference: the original React Bits Stepper with the "onboarding" demo content (StepperDemo.jsx).
// Option, Field, Done, heading and copy are copied from the demo; the state comes from URL params
// (step, name, plan, focus, capture) so each capture loads directly. See ../SPEC.md.
import { useState } from 'react';
import { createRoot } from 'react-dom/client';
import { HugeiconsIcon } from '@hugeicons/react';
import { CheckmarkCircle02Icon } from '@hugeicons/core-free-icons';
import Stepper, { Step } from './original/Stepper.jsx';

const params = new URLSearchParams(location.search);
const initialStep = Number(params.get('step') || 1);
const autoFocus = params.get('focus') === '1';
if (params.has('capture')) document.documentElement.classList.add('capture');

const heading = {
  margin: 0,
  fontSize: 17,
  fontWeight: 600,
  lineHeight: 1.3,
  letterSpacing: '-0.01em',
  color: 'var(--stepper-ink)'
};
const copy = { margin: '6px 0 0', fontSize: 13.5, lineHeight: 1.55, color: 'var(--stepper-muted)' };

const Option = ({ title, detail, aside, selected, onSelect }) => {
  const Tag = onSelect ? 'button' : 'div';
  return (
    <Tag
      type={onSelect ? 'button' : undefined}
      onClick={onSelect}
      style={{
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'space-between',
        gap: 12,
        width: '100%',
        padding: '11px 14px',
        border: 'none',
        borderRadius: 12,
        background: selected ? 'var(--stepper-tile)' : 'var(--stepper-well)',
        boxShadow: selected
          ? 'inset 0 0 0 1px var(--stepper-ring), var(--stepper-tile-shadow)'
          : 'inset 0 0 0 1px var(--stepper-tile-edge)',
        color: 'var(--stepper-ink)',
        font: 'inherit',
        textAlign: 'left',
        boxSizing: 'border-box',
        cursor: onSelect ? 'pointer' : 'default',
        transition: 'background 0.2s ease, box-shadow 0.2s ease'
      }}
    >
      <span style={{ display: 'flex', flexDirection: 'column', gap: 2, minWidth: 0 }}>
        <span style={{ fontSize: 14, fontWeight: 600 }}>{title}</span>
        {detail && <span style={{ fontSize: 12.5, color: 'var(--stepper-muted)' }}>{detail}</span>}
      </span>
      {aside && (
        <span style={{ fontSize: 13, color: 'var(--stepper-muted)', fontVariantNumeric: 'tabular-nums' }}>{aside}</span>
      )}
    </Tag>
  );
};

const Field = ({ value, onChange, placeholder }) => {
  const [focused, setFocused] = useState(false);
  return (
    <input
      value={value}
      onChange={event => onChange(event.target.value)}
      onFocus={() => setFocused(true)}
      onBlur={() => setFocused(false)}
      placeholder={placeholder}
      autoFocus={autoFocus}
      style={{
        width: '100%',
        height: 42,
        marginTop: 14,
        padding: '0 14px',
        boxSizing: 'border-box',
        border: 'none',
        borderRadius: 12,
        outline: 'none',
        background: 'var(--stepper-well)',
        boxShadow: focused ? 'inset 0 0 0 1px var(--stepper-ring)' : 'inset 0 0 0 1px var(--stepper-tile-edge)',
        color: 'var(--stepper-ink)',
        font: 'inherit',
        fontSize: 14,
        transition: 'box-shadow 0.2s ease'
      }}
    />
  );
};

const Done = ({ title, text }) => (
  <div style={{ display: 'flex', alignItems: 'center', gap: 14, padding: '4px 0' }}>
    <span
      style={{
        display: 'grid',
        placeItems: 'center',
        flex: 'none',
        width: 40,
        height: 40,
        borderRadius: 12,
        background: 'var(--stepper-well)',
        color: 'var(--stepper-ink)'
      }}
    >
      <HugeiconsIcon icon={CheckmarkCircle02Icon} size={20} strokeWidth={1.8} />
    </span>
    <span>
      <h3 style={heading}>{title}</h3>
      <p style={{ ...copy, marginTop: 2 }}>{text}</p>
    </span>
  </div>
);

function Demo() {
  const [name, setName] = useState(params.get('name') || '');
  const [plan, setPlan] = useState(params.get('plan') || 'pro');
  const [step, setStep] = useState(initialStep);
  const needsName = step === 3 && !name.trim();
  return (
    <Stepper
      initialStep={initialStep}
      theme="dark"
      indicator="numbers"
      orientation="horizontal"
      transition="slide"
      width={440}
      radius={24}
      frame
      onStepChange={setStep}
      onFinalStepCompleted={() => setStep(5)}
      nextButtonProps={{ disabled: needsName }}
      completedContent={<Done title="Workspace ready" text={`Welcome in${name.trim() ? `, ${name.trim()}` : ''}.`} />}
    >
      <Step key="welcome" title="Welcome">
        <h3 style={heading}>Welcome aboard</h3>
        <p style={copy}>Set up your workspace in a few quick steps. It only takes a minute.</p>
      </Step>
      <Step key="plan" title="Plan">
        <h3 style={heading}>Choose a plan</h3>
        <p style={copy}>You can change this at any time.</p>
        <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 8, marginTop: 14 }}>
          <Option title="Starter" detail="Free forever" selected={plan === 'starter'} onSelect={() => setPlan('starter')} />
          <Option title="Pro" detail="$12 per month" selected={plan === 'pro'} onSelect={() => setPlan('pro')} />
        </div>
      </Step>
      <Step key="profile" title="Profile">
        <h3 style={heading}>What should we call you?</h3>
        <Field value={name} onChange={setName} placeholder="Your name" />
      </Step>
      <Step key="review" title="Review">
        <h3 style={heading}>Looking good{name.trim() ? `, ${name.trim()}` : ''}</h3>
        <p style={copy}>
          You picked the {plan === 'pro' ? 'Pro' : 'Starter'} plan. Complete setup to open your workspace.
        </p>
      </Step>
    </Stepper>
  );
}

createRoot(document.getElementById('root')).render(<Demo />);
