import { engine } from "./engineHost";
import { getPrefs } from "./serverStore";

/**
 * Game activity: notices a known game running and tells the page, which
 * reports it to the server as "Playing …". Only the game's name ever leaves
 * the machine, and only with "Show the game you're playing" on.
 *
 * Matching is by executable name. The built-in list covers widely played PC
 * games; anything else can be added from the Settings window.
 */
const KNOWN_GAMES: Record<string, string> = {
  // Executables are matched case-insensitively.
  "cs2.exe": "Counter-Strike 2",
  "dota2.exe": "Dota 2",
  "valorant-win64-shipping.exe": "VALORANT",
  "leagueoflegends.exe": "League of Legends",
  "league of legends.exe": "League of Legends",
  "fortniteclient-win64-shipping.exe": "Fortnite",
  "r5apex.exe": "Apex Legends",
  "r5apex_dx12.exe": "Apex Legends",
  "overwatch.exe": "Overwatch 2",
  "rocketleague.exe": "Rocket League",
  "minecraft.exe": "Minecraft",
  "minecraftlauncher.exe": "Minecraft Launcher",
  "gta5.exe": "Grand Theft Auto V",
  "gta5_enhanced.exe": "Grand Theft Auto V",
  "rdr2.exe": "Red Dead Redemption 2",
  "eldenring.exe": "ELDEN RING",
  "nightreign.exe": "ELDEN RING NIGHTREIGN",
  "darksoulsiii.exe": "Dark Souls III",
  "sekiro.exe": "Sekiro: Shadows Die Twice",
  "witcher3.exe": "The Witcher 3",
  "cyberpunk2077.exe": "Cyberpunk 2077",
  "bg3.exe": "Baldur's Gate 3",
  "bg3_dx11.exe": "Baldur's Gate 3",
  "starfield.exe": "Starfield",
  "skyrimse.exe": "The Elder Scrolls V: Skyrim",
  "fallout4.exe": "Fallout 4",
  "falloutnv.exe": "Fallout: New Vegas",
  "eurotrucks2.exe": "Euro Truck Simulator 2",
  "amtrucks.exe": "American Truck Simulator",
  "factorio.exe": "Factorio",
  "factorio": "Factorio",
  "terraria.exe": "Terraria",
  "stardew valley.exe": "Stardew Valley",
  "stardewvalley.exe": "Stardew Valley",
  "hollow_knight.exe": "Hollow Knight",
  "hollow knight silksong.exe": "Hollow Knight: Silksong",
  "celeste.exe": "Celeste",
  "hades.exe": "Hades",
  "hades2.exe": "Hades II",
  "deadcells.exe": "Dead Cells",
  "valheim.exe": "Valheim",
  "valheim.x86_64": "Valheim",
  "rust.exe": "Rust",
  "rustclient.exe": "Rust",
  "dayz_x64.exe": "DayZ",
  "arma3_x64.exe": "Arma 3",
  "reforger.exe": "Arma Reforger",
  "escapefromtarkov.exe": "Escape from Tarkov",
  "tslgame.exe": "PUBG: BATTLEGROUNDS",
  "pubg.exe": "PUBG: BATTLEGROUNDS",
  "destiny2.exe": "Destiny 2",
  "warframe.x64.exe": "Warframe",
  "pathofexile.exe": "Path of Exile",
  "pathofexile_x64.exe": "Path of Exile",
  "pathofexile2.exe": "Path of Exile 2",
  "diablo iv.exe": "Diablo IV",
  "wow.exe": "World of Warcraft",
  "wowclassic.exe": "World of Warcraft Classic",
  "ffxiv_dx11.exe": "FINAL FANTASY XIV",
  "gw2-64.exe": "Guild Wars 2",
  "eso64.exe": "The Elder Scrolls Online",
  "lostark.exe": "Lost Ark",
  "newworld.exe": "New World",
  "hearthstone.exe": "Hearthstone",
  "marvelrivals-win64-shipping.exe": "Marvel Rivals",
  "deadlock.exe": "Deadlock",
  "project8.exe": "Deadlock",
  "r6-siege.exe": "Rainbow Six Siege",
  "rainbowsix.exe": "Rainbow Six Siege",
  "rainbowsix_vulkan.exe": "Rainbow Six Siege",
  "cod.exe": "Call of Duty",
  "cod24-cod.exe": "Call of Duty",
  "modernwarfare.exe": "Call of Duty: Modern Warfare",
  "bf2042.exe": "Battlefield 2042",
  "bf6.exe": "Battlefield 6",
  "battlefield6.exe": "Battlefield 6",
  "helldivers2.exe": "HELLDIVERS 2",
  "lethal company.exe": "Lethal Company",
  "phasmophobia.exe": "Phasmophobia",
  "amongus.exe": "Among Us",
  "among us.exe": "Among Us",
  "fallguys_client_game.exe": "Fall Guys",
  "palworld-win64-shipping.exe": "Palworld",
  "enshrouded.exe": "Enshrouded",
  "satisfactory.exe": "Satisfactory",
  "factorygame-win64-shipping.exe": "Satisfactory",
  "noita.exe": "Noita",
  "rimworldwin64.exe": "RimWorld",
  "oxygennotincluded.exe": "Oxygen Not Included",
  "cities.exe": "Cities: Skylines",
  "cities2.exe": "Cities: Skylines II",
  "civilizationvi.exe": "Sid Meier's Civilization VI",
  "civilizationvi_dx12.exe": "Sid Meier's Civilization VI",
  "civ7.exe": "Sid Meier's Civilization VII",
  "eu4.exe": "Europa Universalis IV",
  "hoi4.exe": "Hearts of Iron IV",
  "stellaris.exe": "Stellaris",
  "ck3.exe": "Crusader Kings III",
  "aoe2de_s.exe": "Age of Empires II: Definitive Edition",
  "sc2_x64.exe": "StarCraft II",
  "slay the spire.exe": "Slay the Spire",
  "balatro.exe": "Balatro",
  "vampiresurvivors.exe": "Vampire Survivors",
  "risk of rain 2.exe": "Risk of Rain 2",
  "deeprockgalactic.exe": "Deep Rock Galactic",
  "fsd-win64-shipping.exe": "Deep Rock Galactic",
  "left4dead2.exe": "Left 4 Dead 2",
  "hl2.exe": "Half-Life 2",
  "portal2.exe": "Portal 2",
  "tf_win64.exe": "Team Fortress 2",
  "hl.exe": "Half-Life",
  "gmod.exe": "Garry's Mod",
  "gmod64.exe": "Garry's Mod",
  "theforest.exe": "The Forest",
  "sonsoftheforest.exe": "Sons of the Forest",
  "subnautica.exe": "Subnautica",
  "nomanssky.exe": "No Man's Sky",
  "seaofthieves.exe": "Sea of Thieves",
  "forzahorizon5.exe": "Forza Horizon 5",
  "acc.exe": "Assetto Corsa Competizione",
  "iracingsim64dx11.exe": "iRacing",
  "beamng.drive.x64.exe": "BeamNG.drive",
  "osu!.exe": "osu!",
  "geometrydash.exe": "Geometry Dash",
  "dontstarve_steam.exe": "Don't Starve",
  "dontstarve_steam_x64.exe": "Don't Starve Together",
  "projectzomboid64.exe": "Project Zomboid",
  "7daystodie.exe": "7 Days to Die",
  "ark.exe": "ARK",
  "arkascended.exe": "ARK: Survival Ascended",
  "monsterhunterwilds.exe": "Monster Hunter Wilds",
  "monsterhunterworld.exe": "Monster Hunter: World",
  "re4.exe": "Resident Evil 4",
  "baldursgate3.exe": "Baldur's Gate 3",
  "hogwartslegacy.exe": "Hogwarts Legacy",
  "thefinals.exe": "THE FINALS",
  "discovery.exe": "THE FINALS",
  "brawlhalla.exe": "Brawlhalla",
  "smite.exe": "SMITE",
  "paladins.exe": "Paladins",
  "robloxplayerbeta.exe": "Roblox",
  "genshinimpact.exe": "Genshin Impact",
  "starrail.exe": "Honkai: Star Rail",
  "zenlesszonezero.exe": "Zenless Zone Zero",
  "wutheringwaves.exe": "Wuthering Waves",
};

export function gameFor(exe: string, extra: { exe: string; name: string }[]): string | null {
  const key = exe.toLowerCase();
  const added = extra.find((g) => g.exe.toLowerCase() === key);
  return added?.name ?? KNOWN_GAMES[key] ?? null;
}

let current: string | null = null;
const listeners = new Set<(game: string | null) => void>();

export function currentGame(): string | null {
  return current;
}

export function onGameChanged(listener: (game: string | null) => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

async function scan(): Promise<void> {
  const prefs = getPrefs();
  let found: string | null = null;
  if (prefs.shareGameActivity) {
    // While the engine restarts there's no answer: keep what we last knew
    // rather than report that the game closed.
    const processes = engine.hello
      ? ((await engine.request("processes.list").catch(() => null)) as { exe: string }[] | null)
      : null;
    if (!processes) return;
    for (const p of processes) {
      found = gameFor(p.exe, prefs.extraGames);
      if (found) break;
    }
  }
  if (found !== current) {
    current = found;
    listeners.forEach((l) => l(found));
  }
}

export function startGameDetection(): void {
  void scan();
  setInterval(() => void scan(), 15_000);
}

/** Re-check now (after a settings change) rather than on the next tick. */
export function rescanGames(): void {
  void scan();
}
