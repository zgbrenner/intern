import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { BrowserApp } from './BrowserApp';
import './styles/base.css';

const root = document.getElementById('root');

if (!root) {
  throw new Error('Intern could not find its root element.');
}

createRoot(root).render(
  <StrictMode>
    <BrowserApp />
  </StrictMode>,
);
