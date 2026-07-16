'use strict';

window.showStartupError = (message) => {
  document.body.classList.add('error');
  document.querySelector('h1').textContent = 'Could not open the vault';
  document.querySelector('#status').textContent = message;
};
