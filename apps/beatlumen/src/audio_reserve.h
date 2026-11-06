// Asking the sound server for a card before opening it, over the session bus
// (org.freedesktop.ReserveDevice1). PipeWire and PulseAudio hold every card
// they manage; whoever owns the bus name of a card has it.
#pragma once

#include <QString>

#include <functional>

class ReservationObject;

class CardReservation
{
public:
    ~CardReservation();
    // Takes card `number` (nothing to do when it is already held). True also
    // when there is no session bus: then there is nobody to ask.
    bool take(int number, QString *err);
    // Gives the card back; the server takes it again by itself.
    void release();
    // Somebody with a better claim asked for the card (from another thread).
    static void onAskedToLetGo(std::function<void()> handler);
    static void shutdown();

private:
    int m_card = -1;
    ReservationObject *m_object = nullptr;
};
