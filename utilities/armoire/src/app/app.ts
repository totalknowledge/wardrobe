import { Component } from '@angular/core';
import { RouterOutlet } from '@angular/router';
import { DialogComponent } from './dialog/dialog';
import { FooterComponent } from './footer/footer';
import { HeaderComponent } from './header/header';

@Component({
  selector: 'app-root',
  imports: [RouterOutlet, HeaderComponent, FooterComponent, DialogComponent],
  templateUrl: './app.html',
  styleUrl: './app.scss'
})
export class AppComponent {
  constructor() {}

  public handleAppClick(): void {
  }
}
