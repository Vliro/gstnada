//
// Created by albert on 2022-02-22.
//

#include "nada-controller.h"
using rmcat::NadaController;
extern "C" {
NadaController* NewController(int32_t minbw, int32_t maxbw) {
    NadaController* controller = new NadaController();
    controller->setMinBw(static_cast<float>(minbw*1000));
    controller->setMaxBw(static_cast<float>(maxbw*1000));
    return controller;
}

void FreeController(NadaController* c) {
    delete c;
}

bool OnFeedback(NadaController* controller, uint64_t nowUs,
                uint16_t sequence,
                uint64_t rxTimestampUs,
                uint8_t ecn) {
    return controller->processFeedback(nowUs, sequence, rxTimestampUs, ecn);
}

bool OnPacket(NadaController* controller, uint64_t txTimestampUs,
              uint16_t sequence,
              uint32_t size) {
    return controller->processSendPacket(txTimestampUs, sequence, size);
}

float getBitrate(NadaController* controller, uint64_t nowUs) {
    return controller->getBandwidth(nowUs);
}
}
